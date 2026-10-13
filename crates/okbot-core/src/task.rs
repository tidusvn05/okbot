//! The task port: to-dos, reminders and routines (design §5.6).
//!
//! A task is one file in the knowledge bundle; this module holds the model, the repeat rules and
//! the [`TaskStore`] trait. Times are [`Timestamp`]s; rules and local input are read in the bot's
//! time zone.

use std::fmt;
use std::str::FromStr;

use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan as _};
use serde::{Deserialize, Serialize};

use crate::id::{ChannelId, ConversationKey};
use crate::knowledge::Requester;
use crate::memory::MemoryScope;
use crate::{BoxError, async_trait};

/// What a task does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// Something to do; a reminder, if any, does not close it.
    Todo,
    /// A message at a time; one-shot reminders are done once sent.
    Reminder,
    /// An agent turn at a time, with the notes as the prompt (design §5.7).
    Routine,
}

impl TaskKind {
    /// The name used in files and tools.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Reminder => "reminder",
            Self::Routine => "routine",
        }
    }
}

impl FromStr for TaskKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "todo" => Ok(Self::Todo),
            "reminder" => Ok(Self::Reminder),
            "routine" => Ok(Self::Routine),
            other => Err(format!(
                "unknown kind `{other}` (todo, reminder or routine)"
            )),
        }
    }
}

/// Where a task is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    /// Waiting to be done or to fire.
    Open,
    /// Kept but not firing (routines waiting for confirmation, paused by the user).
    Paused,
    /// Finished.
    Done,
    /// Dropped.
    Cancelled,
}

impl TaskStatus {
    /// The name used in files and tools.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    /// Open or paused.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Open | Self::Paused)
    }
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "open" => Ok(Self::Open),
            "paused" => Ok(Self::Paused),
            "done" => Ok(Self::Done),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            other => Err(format!(
                "unknown status `{other}` (open, paused, done or cancelled)"
            )),
        }
    }
}

/// When a task fires again, written `daily 08:00`, `weekdays 08:00`, `weekly mon,fri 09:00`,
/// `monthly 1 09:00` or `every 3h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    /// Every day at a local time.
    Daily(Time),
    /// Monday to Friday at a local time.
    Weekdays(Time),
    /// On some days of the week (Monday first, no duplicates) at a local time.
    Weekly(Vec<Weekday>, Time),
    /// On a day of the month (the last day in shorter months) at a local time.
    Monthly(i8, Time),
    /// Every few hours after the previous firing (1 to 168).
    EveryHours(i64),
}

const DAYS: [(&str, Weekday); 7] = [
    ("mon", Weekday::Monday),
    ("tue", Weekday::Tuesday),
    ("wed", Weekday::Wednesday),
    ("thu", Weekday::Thursday),
    ("fri", Weekday::Friday),
    ("sat", Weekday::Saturday),
    ("sun", Weekday::Sunday),
];

fn parse_clock(s: &str) -> Result<Time, String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("`{s}` must be a time such as 08:00"))?;
    let h: i8 = h.parse().map_err(|_| format!("`{s}` is not a time"))?;
    let m: i8 = m.parse().map_err(|_| format!("`{s}` is not a time"))?;
    Time::new(h, m, 0, 0).map_err(|_| format!("`{s}` is not a time"))
}

fn day_name(day: Weekday) -> &'static str {
    DAYS.iter()
        .find(|(_, d)| *d == day)
        .map_or("mon", |(name, _)| name)
}

impl FromStr for Repeat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.trim().to_ascii_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        match words.as_slice() {
            ["daily", at] => Ok(Self::Daily(parse_clock(at)?)),
            ["weekdays", at] => Ok(Self::Weekdays(parse_clock(at)?)),
            ["weekly", days, at] => {
                let mut list = Vec::new();
                for name in days.split(',') {
                    let day = DAYS
                        .iter()
                        .find(|(n, _)| name.starts_with(n))
                        .map(|(_, d)| *d)
                        .ok_or_else(|| format!("`{name}` is not a day (mon … sun)"))?;
                    if !list.contains(&day) {
                        list.push(day);
                    }
                }
                list.sort_by_key(|d| d.to_monday_one_offset());
                Ok(Self::Weekly(list, parse_clock(at)?))
            }
            ["monthly", day, at] => {
                let day: i8 = day
                    .parse()
                    .ok()
                    .filter(|d| (1..=31).contains(d))
                    .ok_or_else(|| format!("`{day}` is not a day of the month (1 to 31)"))?;
                Ok(Self::Monthly(day, parse_clock(at)?))
            }
            ["every", hours] => {
                let n: i64 = hours
                    .strip_suffix('h')
                    .and_then(|n| n.parse().ok())
                    .filter(|n| (1..=168).contains(n))
                    .ok_or_else(|| format!("`every {hours}` must be 1h to 168h"))?;
                Ok(Self::EveryHours(n))
            }
            _ => Err(format!(
                "`{s}` is not a repeat rule: use `daily 08:00`, `weekdays 08:00`, \
                 `weekly mon,fri 09:00`, `monthly 1 09:00` or `every 3h`"
            )),
        }
    }
}

impl fmt::Display for Repeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let clock = |t: &Time| format!("{:02}:{:02}", t.hour(), t.minute());
        match self {
            Self::Daily(t) => write!(f, "daily {}", clock(t)),
            Self::Weekdays(t) => write!(f, "weekdays {}", clock(t)),
            Self::Weekly(days, t) => {
                let names: Vec<&str> = days.iter().map(|d| day_name(*d)).collect();
                write!(f, "weekly {} {}", names.join(","), clock(t))
            }
            Self::Monthly(day, t) => write!(f, "monthly {day} {}", clock(t)),
            Self::EveryHours(n) => write!(f, "every {n}h"),
        }
    }
}

impl Repeat {
    fn matches(&self, date: Date) -> bool {
        match self {
            Self::Weekdays(_) => !matches!(date.weekday(), Weekday::Saturday | Weekday::Sunday),
            Self::Weekly(days, _) => days.contains(&date.weekday()),
            Self::Monthly(day, _) => date.day() == (*day).min(date.days_in_month()),
            Self::Daily(_) | Self::EveryHours(_) => true,
        }
    }

    /// The first firing strictly after `after`, in `tz` (local times that do not exist on a DST
    /// change move forward).
    pub fn next_after(&self, after: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
        let at = match self {
            Self::EveryHours(n) => return after.checked_add(n.hours()).ok(),
            Self::Daily(t) | Self::Weekdays(t) | Self::Weekly(_, t) | Self::Monthly(_, t) => *t,
        };
        let mut date = after.to_zoned(tz.clone()).date();
        // Monthly rules need at most a month; 400 days covers every rule with margin.
        for _ in 0..400 {
            if self.matches(date) {
                let when = date.to_datetime(at).to_zoned(tz.clone()).ok()?.timestamp();
                if when > after {
                    return Some(when);
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }
}

/// Reads a time written by a user or an agent: an RFC 3339 instant (`2026-10-14T08:00:00+07:00`),
/// a local date-time in `tz` (`2026-10-14T08:00`, `2026-10-14 08:00`) or a local date (09:00).
pub fn parse_time(text: &str, tz: &TimeZone) -> Result<Timestamp, String> {
    let text = text.trim();
    if let Ok(ts) = text.parse::<Timestamp>() {
        return Ok(ts);
    }
    let local = text.replacen(' ', "T", 1);
    // A date alone means 09:00 (jiff would read it as midnight).
    let datetime = if local.contains('T') {
        local.parse::<DateTime>()
    } else {
        local
            .parse::<Date>()
            .map(|d| d.to_datetime(Time::constant(9, 0, 0, 0)))
    };
    datetime
        .ok()
        .and_then(|dt| dt.to_zoned(tz.clone()).ok())
        .map(|z| z.timestamp())
        .ok_or_else(|| {
            format!("`{text}` is not a time: use `YYYY-MM-DD HH:MM` (local) or RFC 3339")
        })
}

/// Formats `ts` as an RFC 3339 instant with the offset of `tz` (`2026-10-14T08:00:00+07:00`).
pub fn format_time(ts: Timestamp, tz: &TimeZone) -> String {
    ts.to_zoned(tz.clone())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// A stored task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Id in the knowledge bundle (`tasks/people/owner/t-7k2m9q`).
    pub id: String,
    /// What it does.
    pub kind: TaskKind,
    /// Where it is in its life.
    pub status: TaskStatus,
    /// Short title.
    pub title: String,
    /// Free notes; the prompt of a routine.
    pub notes: String,
    /// Personal (`User`) or for the room (`Room`).
    pub scope: MemoryScope,
    /// When it should be done.
    pub due: Option<Timestamp>,
    /// The next firing.
    pub remind_at: Option<Timestamp>,
    /// How it repeats.
    pub repeat: Option<Repeat>,
    /// Where it fires: `None` for the conversation it was created in, or a channel (the owner's
    /// direct chat there).
    pub deliver_to: Option<ChannelId>,
    /// The conversation it was created in.
    pub origin: Option<ConversationKey>,
    /// `platform:instance/user` of the author.
    pub author: String,
    /// When it was created.
    pub created: Timestamp,
}

/// A task to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    /// What it does.
    pub kind: TaskKind,
    /// Short title.
    pub title: String,
    /// Free notes; the prompt of a routine.
    pub notes: String,
    /// Personal (`User`) or for the room (`Room`); `Global` is refused.
    pub scope: MemoryScope,
    /// When it should be done.
    pub due: Option<Timestamp>,
    /// The first firing.
    pub remind_at: Option<Timestamp>,
    /// How it repeats.
    pub repeat: Option<Repeat>,
    /// Where it fires (`None`: here).
    pub deliver_to: Option<ChannelId>,
    /// Created paused (routines that need confirmation).
    pub paused: bool,
}

impl NewTask {
    /// A personal to-do with only a title.
    pub fn todo(title: impl Into<String>) -> Self {
        Self {
            kind: TaskKind::Todo,
            title: title.into(),
            notes: String::new(),
            scope: MemoryScope::User,
            due: None,
            remind_at: None,
            repeat: None,
            deliver_to: None,
            paused: false,
        }
    }
}

/// Changes to a task; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskChange {
    /// New title.
    pub title: Option<String>,
    /// New notes.
    pub notes: Option<String>,
    /// New status.
    pub status: Option<TaskStatus>,
    /// New due time (`Some(None)` clears it).
    pub due: Option<Option<Timestamp>>,
    /// New next firing (`Some(None)` clears it).
    pub remind_at: Option<Option<Timestamp>>,
    /// New repeat rule (`Some(None)` clears it).
    pub repeat: Option<Option<Repeat>>,
}

/// A task operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskError {
    /// No such task in the requester's scope.
    #[error("no task `{0}` is visible here")]
    NotFound(String),
    /// The requester may not do this.
    #[error("not allowed: {0}")]
    Forbidden(String),
    /// The request is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// Storage failed.
    #[error("task storage: {0}")]
    Backend(#[source] BoxError),
}

/// Creates, lists and changes tasks, always within the requester's visibility.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Creates a task.
    async fn add(&self, who: &Requester, task: NewTask) -> Result<Task, TaskError>;

    /// Tasks visible to `who` (theirs and their room's), every status, in display order: active
    /// first, then by next firing or due time, then by creation.
    async fn list(&self, who: &Requester) -> Result<Vec<Task>, TaskError>;

    /// Changes a visible task.
    async fn update(
        &self,
        who: &Requester,
        id: &str,
        change: TaskChange,
    ) -> Result<Task, TaskError>;

    /// Deletes a visible task.
    async fn delete(&self, who: &Requester, id: &str) -> Result<Task, TaskError>;
}

/// Sorts tasks for display: active first, then by the earliest of next firing and due time, then
/// by creation.
pub fn sort_tasks(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        let when = |t: &Task| match (t.remind_at, t.due) {
            (Some(r), Some(d)) => Some(r.min(d)),
            (r, d) => r.or(d),
        };
        let key = |t: &Task| (!t.status.is_active(), when(t).is_none(), when(t), t.created);
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hcm() -> TimeZone {
        TimeZone::get("Asia/Ho_Chi_Minh").unwrap()
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn repeat_rules_round_trip() {
        for rule in [
            "daily 08:00",
            "weekdays 07:30",
            "weekly mon,fri 09:00",
            "monthly 31 18:05",
            "every 3h",
        ] {
            assert_eq!(rule.parse::<Repeat>().unwrap().to_string(), rule);
        }
        assert_eq!(
            "Weekly FRI,monday,fri 9:00"
                .parse::<Repeat>()
                .unwrap()
                .to_string(),
            "weekly mon,fri 09:00"
        );
        for bad in [
            "daily",
            "daily 25:00",
            "every 0h",
            "every 2d",
            "monthly 32 08:00",
            "x",
        ] {
            assert!(bad.parse::<Repeat>().is_err(), "{bad}");
        }
    }

    #[test]
    fn next_firings() {
        let tz = hcm();
        let daily: Repeat = "daily 08:00".parse().unwrap();
        // 2026-10-13 07:00 local → same day 08:00; 08:00 exactly → next day.
        assert_eq!(
            daily.next_after(at("2026-10-13T00:00:00Z"), &tz),
            Some(at("2026-10-13T01:00:00Z"))
        );
        assert_eq!(
            daily.next_after(at("2026-10-13T01:00:00Z"), &tz),
            Some(at("2026-10-14T01:00:00Z"))
        );
        // Friday 2026-10-16 after 09:00 → Monday 2026-10-19.
        let weekdays: Repeat = "weekdays 09:00".parse().unwrap();
        assert_eq!(
            weekdays.next_after(at("2026-10-16T03:00:00Z"), &tz),
            Some(at("2026-10-19T02:00:00Z"))
        );
        // The 31st in a 30-day month is the 30th.
        let monthly: Repeat = "monthly 31 08:00".parse().unwrap();
        assert_eq!(
            monthly.next_after(at("2026-11-01T00:00:00Z"), &tz),
            Some(at("2026-11-30T01:00:00Z"))
        );
        let every: Repeat = "every 3h".parse().unwrap();
        assert_eq!(
            every.next_after(at("2026-10-13T00:00:00Z"), &tz),
            Some(at("2026-10-13T03:00:00Z"))
        );
    }

    #[test]
    fn dst_gap_moves_forward() {
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        // 2027-03-28 02:30 does not exist in Berlin; the firing is at 03:30 local (01:30Z).
        let daily: Repeat = "daily 02:30".parse().unwrap();
        assert_eq!(
            daily.next_after(at("2027-03-27T12:00:00Z"), &tz),
            Some(at("2027-03-28T01:30:00Z"))
        );
    }

    #[test]
    fn parses_local_and_absolute_times() {
        let tz = hcm();
        let want = at("2026-10-14T01:00:00Z");
        assert_eq!(parse_time("2026-10-14T08:00", &tz), Ok(want));
        assert_eq!(parse_time("2026-10-14 08:00", &tz), Ok(want));
        assert_eq!(parse_time("2026-10-14T08:00:00+07:00", &tz), Ok(want));
        assert_eq!(
            parse_time("2026-10-14", &tz),
            Ok(at("2026-10-14T02:00:00Z"))
        );
        assert!(parse_time("tomorrow", &tz).is_err());
        assert_eq!(format_time(want, &tz), "2026-10-14T08:00:00+07:00");
    }
}

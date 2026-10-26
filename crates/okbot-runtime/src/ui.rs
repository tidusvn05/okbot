//! Interactive replies (ADR 0015): the agent's `show` tool on the built-in `ui` server of the
//! gateway, how a view becomes a prompt on each platform (or text where the platform lacks a
//! part), and how a press, a pick or a form becomes the clicker's next message.
//!
//! Views are sent after the answer text. Those with something to answer are kept in the store
//! until [`TTL`], so their buttons outlive restarts; the first answer wins and closes them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use okbot_core::{
    ButtonStyle, Card, ChannelCaps, Chart, ChartKind, ConversationKey, FieldStyle, Form, FormField,
    Image, InboundEvent, InboundSink as _, Prompt, PromptAnswer, PromptButton, ReplyMode,
    ReplyTarget, Select, SelectOption, SentRef, Series, Tone, ToolCallRecord, ToolDecision,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Shared;
use crate::gateway::Grant;

/// The gateway path and the server name the agent sees.
pub(crate) const SERVER: &str = "ui";
/// The variable carrying the agent's gateway token for the `ui` server.
pub(crate) const TOKEN_ENV: &str = "OKBOT_UI_TOKEN";
/// How long a view waits for an answer.
const TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Views one answer may show.
const MAX_VIEWS: usize = 3;
/// Choices of one view (a menu holds 25).
const MAX_CHOICES: usize = 25;
/// Link buttons of one view.
const MAX_LINKS: usize = 3;
/// Rows of a table shown; the rest is counted.
const MAX_ROWS: usize = 15;
/// Characters of the text before the cards, and of one card's text.
const MAX_TEXT: usize = 1000;
const MAX_CARD_TEXT: usize = 1500;
/// Width of the longest bar of a chart.
const BAR_WIDTH: usize = 12;
/// How long drawing a chart may take before it is shown as text (ADR 0016).
const CHART_TIMEOUT: Duration = Duration::from_secs(2);
/// The id of a form button (choices are `b1`…, options `o1`…, links `l1`…).
const FORM: &str = "form";

/// The views one turn's agent asked to show, sent after its answer.
pub(crate) type Shown = Arc<Mutex<Vec<View>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether a turn answering `event` may show views: the room is interactive, the agent can call
/// MCP tools, and the turn is neither an automation's (its data is untrusted, ADR 0013) nor
/// another bot's task (its answer is text for that bot).
pub(crate) fn enabled(shared: &Shared, event: &InboundEvent, restricted: bool) -> bool {
    !restricted
        && !crate::team::is_peer(event)
        && shared.agent.caps().mcp
        && shared
            .config
            .reply_mode(&event.conversation.channel, &event.conversation.room)
            == ReplyMode::Interactive
}

/// The agent's arguments to `show`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    #[serde(default)]
    text: String,
    #[serde(default)]
    cards: Vec<CardArgs>,
    #[serde(default)]
    choices: Vec<ChoiceArgs>,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    form: Option<FormArgs>,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CardArgs {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tone: Tone,
    #[serde(default)]
    text: String,
    #[serde(default)]
    fields: Vec<FieldArgs>,
    #[serde(default)]
    table: Option<TableArgs>,
    #[serde(default)]
    chart: Option<ChartArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldArgs {
    label: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableArgs {
    #[serde(default)]
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChartArgs {
    #[serde(default)]
    kind: ChartKind,
    #[serde(default)]
    unit: Option<String>,
    /// One series: a value per label.
    #[serde(default)]
    points: Vec<PointArgs>,
    /// Several series: the labels, then the series' values in that order.
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    series: Vec<SeriesArgs>,
    #[serde(default)]
    highlight: Option<String>,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeriesArgs {
    #[serde(default)]
    name: Option<String>,
    values: Vec<Option<f64>>,
}

impl ChartArgs {
    /// The chart, checked against the limits of its kind.
    fn chart(&self, tone: Tone) -> Result<Chart, String> {
        let (labels, series): (Vec<String>, Vec<Series>) =
            match (self.points.is_empty(), self.series.is_empty()) {
                (false, true) if self.labels.is_empty() => (
                    self.points
                        .iter()
                        .map(|p| p.label.trim().to_string())
                        .collect(),
                    vec![Series {
                        name: None,
                        values: self.points.iter().map(|p| Some(p.value)).collect(),
                    }],
                ),
                (true, false) => (
                    self.labels.iter().map(|l| l.trim().to_string()).collect(),
                    self.series
                        .iter()
                        .map(|s| Series {
                            name: s.name.clone(),
                            values: s.values.clone(),
                        })
                        .collect(),
                ),
                _ => return Err("a chart has either points, or labels and series".into()),
            };
        let highlight = match &self.highlight {
            None => None,
            Some(h) => Some(
                labels
                    .iter()
                    .position(|l: &String| l == h.trim())
                    .ok_or("the highlight must be one of the chart's labels")?,
            ),
        };
        let chart = Chart {
            kind: self.kind,
            title: None,
            unit: self.unit.clone().filter(|u| !u.trim().is_empty()),
            labels,
            series,
            highlight,
            tone,
            locale: self.locale.clone(),
        };
        chart.validate()?;
        Ok(chart)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PointArgs {
    label: String,
    value: f64,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChoiceArgs {
    Label(String),
    Full {
        label: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        style: Option<ButtonStyle>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormArgs {
    #[serde(default)]
    button: Option<String>,
    title: String,
    fields: Vec<FormFieldArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormFieldArgs {
    label: String,
    #[serde(default)]
    multiline: bool,
    #[serde(default = "yes")]
    required: bool,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

fn yes() -> bool {
    true
}

/// A view as the agent described it, in canonical Markdown: what [`build`] turns into a prompt
/// for one platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct View {
    text: String,
    cards: Vec<ViewCard>,
    choices: Vec<Choice>,
    placeholder: Option<String>,
    form: Option<ViewForm>,
    links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ViewCard {
    title: Option<String>,
    tone: Tone,
    /// Text, fields and table.
    markdown: String,
    chart: Option<Chart>,
}

impl ViewCard {
    /// The body in chat lines, with the chart as text unless it is shown as a picture.
    fn body(&self, chart_as_text: bool) -> String {
        let chart = self
            .chart
            .as_ref()
            .filter(|_| chart_as_text)
            .map(chart_markdown)
            .unwrap_or_default();
        [self.markdown.as_str(), chart.as_str()]
            .into_iter()
            .filter(|b| !b.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Choice {
    label: String,
    description: Option<String>,
    style: ButtonStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ViewForm {
    button: String,
    form: Form,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    label: String,
    url: String,
}

/// Field labels that ask for a secret: a form's values reach the agent and the history.
const SECRET_WORDS: [&str; 14] = [
    "password",
    "passcode",
    "passphrase",
    "mật khẩu",
    "otp",
    "token",
    "api key",
    "secret",
    "cvv",
    "cvc",
    "pin",
    "card number",
    "số thẻ",
    "mã xác",
];

fn asks_secret(label: &str) -> bool {
    let label = label.to_lowercase();
    SECRET_WORDS.iter().any(|w| {
        label
            .split(|c: char| !c.is_alphanumeric() && c != ' ')
            .any(|part| part.trim() == *w)
            || (w.contains(' ') && label.contains(w))
            || label.split_whitespace().any(|word| word == *w)
    })
}

fn too_long(what: &str, text: &str, max: usize) -> Result<(), String> {
    if text.chars().count() > max {
        Err(format!("{what}: at most {max} characters"))
    } else {
        Ok(())
    }
}

/// A cell of a table as text.
fn cell(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A number without needless decimals.
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

impl CardArgs {
    /// The card's body: text, then fields and a table, each in chat lines.
    fn markdown(&self) -> String {
        let mut out = self.text.trim().to_string();
        let mut add = |block: String| {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&block);
        };
        if !self.fields.is_empty() {
            let lines: Vec<String> = self
                .fields
                .iter()
                .map(|f| format!("**{}:** {}", f.label.trim(), f.value.trim()))
                .collect();
            add(lines.join("\n"));
        }
        if let Some(table) = &self.table {
            let mut lines = Vec::new();
            if !table.columns.is_empty() && table.columns.len() <= 4 {
                lines.push(format!("-# {}", table.columns.join(" · ")));
            }
            for row in table.rows.iter().take(MAX_ROWS) {
                let mut cells = row.iter().map(cell);
                let first = cells.next().unwrap_or_default();
                let rest: Vec<String> = cells.collect();
                let mut line = format!("• **{first}**");
                for c in rest {
                    let _ = write!(line, " · {c}");
                }
                lines.push(line);
            }
            if table.rows.len() > MAX_ROWS {
                lines.push(format!("… {} more rows", table.rows.len() - MAX_ROWS));
            }
            add(lines.join("\n"));
        }
        out
    }
}

/// A chart in chat lines: one bar of `▇` per label, or a value per series.
fn chart_markdown(chart: &Chart) -> String {
    let unit = chart
        .unit
        .as_deref()
        .map(|u| format!(" {u}"))
        .unwrap_or_default();
    let value = |v: &Option<f64>| v.map_or_else(|| "—".to_string(), number);
    let mut lines = Vec::new();
    if let [series] = chart.series.as_slice() {
        let max = series
            .values
            .iter()
            .flatten()
            .map(|v| v.abs())
            .fold(0.0_f64, f64::max);
        for (label, v) in chart.labels.iter().zip(&series.values).take(MAX_ROWS) {
            let Some(v) = v else {
                lines.push(format!("{label} —"));
                continue;
            };
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )] // a width from 0 to BAR_WIDTH
            let width = if max > 0.0 {
                ((v.abs() / max) * BAR_WIDTH as f64).round() as usize
            } else {
                0
            };
            let bar = "▇".repeat(width.max(1));
            lines.push(format!("{label} `{bar}` {}{unit}", number(*v)));
        }
    } else {
        let names: Vec<&str> = chart
            .series
            .iter()
            .map(|s| s.name.as_deref().unwrap_or_default())
            .collect();
        lines.push(format!("-# {}{unit}", names.join(" · ")));
        for (i, label) in chart.labels.iter().enumerate().take(MAX_ROWS) {
            let mut line = format!("• **{label}**");
            for s in &chart.series {
                let _ = write!(line, " · {}", value(&s.values[i]));
            }
            lines.push(line);
        }
    }
    if chart.labels.len() > MAX_ROWS {
        lines.push(format!("… {} more", chart.labels.len() - MAX_ROWS));
    }
    lines.join("\n")
}

impl View {
    /// Checks the agent's arguments; the error tells it what to change.
    fn from_args(args: ShowArgs) -> Result<Self, String> {
        too_long("text", &args.text, MAX_TEXT)?;
        if args.cards.len() > okbot_core::MAX_CARDS {
            return Err(format!("at most {} cards", okbot_core::MAX_CARDS));
        }
        let mut cards = Vec::new();
        for c in &args.cards {
            let chart = c.chart.as_ref().map(|a| a.chart(c.tone)).transpose()?;
            let card = ViewCard {
                title: c.title.clone().filter(|t| !t.trim().is_empty()),
                tone: c.tone,
                markdown: c.markdown(),
                chart,
            };
            let markdown = card.body(true);
            too_long(
                "card text with its fields, table and chart",
                &markdown,
                MAX_CARD_TEXT,
            )?;
            if let Some(title) = &c.title {
                too_long("card title", title, 100)?;
            }
            if markdown.is_empty() && c.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
                return Err("a card needs a title or content".into());
            }
            cards.push(card);
        }
        if args.choices.len() > MAX_CHOICES || args.choices.len() == 1 {
            return Err(format!(
                "choices: 2 to {MAX_CHOICES} (a single action is a choice too: add the alternative)"
            ));
        }
        let mut choices = Vec::new();
        for c in args.choices {
            let (label, description, style) = match c {
                ChoiceArgs::Label(label) => (label, None, None),
                ChoiceArgs::Full {
                    label,
                    description,
                    style,
                } => (label, description, style),
            };
            if label.trim().is_empty() {
                return Err("choice labels must not be empty".into());
            }
            too_long("choice label", &label, 80)?;
            if let Some(d) = &description {
                too_long("choice description", d, 100)?;
            }
            if choices.iter().any(|c: &Choice| c.label == label) {
                return Err(format!("choice {label:?} is listed twice"));
            }
            choices.push(Choice {
                label,
                description,
                style: style.unwrap_or(ButtonStyle::Secondary),
            });
        }
        if let Some(p) = &args.placeholder {
            too_long("placeholder", p, 150)?;
        }
        let form = args.form.map(ViewForm::from_args).transpose()?;
        if args.links.len() > MAX_LINKS {
            return Err(format!("at most {MAX_LINKS} links"));
        }
        for l in &args.links {
            too_long("link label", &l.label, 80)?;
            if l.label.trim().is_empty()
                || !l.url.starts_with("https://")
                || l.url.len() > 512
                || l.url.contains(char::is_whitespace)
            {
                return Err("links need a label and an https URL".into());
            }
        }
        let view = Self {
            text: args.text.trim().to_string(),
            cards,
            choices,
            placeholder: args.placeholder,
            form,
            links: args.links,
        };
        if view.text.is_empty()
            && view.cards.is_empty()
            && view.choices.is_empty()
            && view.form.is_none()
            && view.links.is_empty()
        {
            return Err("nothing to show".into());
        }
        Ok(view)
    }

    /// The form's fields as text (for the history, and platforms without forms).
    fn form_markdown(form: &ViewForm) -> String {
        let fields: Vec<String> = form
            .form
            .fields
            .iter()
            .map(|f| {
                let mut line = format!("- {}", f.label);
                if !f.required {
                    line.push_str(" (optional)");
                }
                if let Some(v) = &f.value {
                    let _ = write!(line, ": {v}");
                }
                line
            })
            .collect();
        format!("**{}**\n{}", form.form.title, fields.join("\n"))
    }

    fn card_markdown(card: &ViewCard) -> String {
        let mut out = String::new();
        if let Some(title) = &card.title {
            let emoji = card.tone.emoji();
            let _ = write!(
                out,
                "**{}{}{title}**",
                emoji,
                if emoji.is_empty() { "" } else { " " }
            );
        }
        let body = card.body(true);
        if !body.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&body);
        }
        out
    }

    fn choices_markdown(&self) -> String {
        self.choices
            .iter()
            .enumerate()
            .map(|(i, c)| match &c.description {
                Some(d) => format!("{}. {} — {d}", i + 1, c.label),
                None => format!("{}. {}", i + 1, c.label),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn links_markdown(&self) -> String {
        self.links
            .iter()
            .map(|l| format!("- {}: {}", l.label, l.url))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Everything as chat text: what the history keeps, so later turns know what was shown.
    pub(crate) fn markdown(&self) -> String {
        let mut blocks = vec![self.text.clone()];
        blocks.extend(self.cards.iter().map(Self::card_markdown));
        blocks.push(self.choices_markdown());
        blocks.extend(self.form.iter().map(Self::form_markdown));
        blocks.push(self.links_markdown());
        join(blocks)
    }
}

fn join(blocks: Vec<String>) -> String {
    blocks
        .into_iter()
        .filter(|b| !b.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl ViewForm {
    fn from_args(args: FormArgs) -> Result<Self, String> {
        let button = args.button.unwrap_or_else(|| args.title.clone());
        too_long("form button", &button, 80)?;
        too_long("form title", &args.title, 45)?;
        if args.title.trim().is_empty() {
            return Err("a form needs a title".into());
        }
        if args.fields.is_empty() || args.fields.len() > okbot_core::MAX_FIELDS {
            return Err(format!("a form has 1 to {} fields", okbot_core::MAX_FIELDS));
        }
        let mut fields = Vec::new();
        for (i, f) in args.fields.into_iter().enumerate() {
            if f.label.trim().is_empty() {
                return Err("form fields need a label".into());
            }
            too_long("field label", &f.label, 45)?;
            if asks_secret(&f.label) {
                return Err(format!(
                    "field {:?}: never ask for passwords, codes, tokens or card numbers in a form; \
                     what people type here reaches you and the chat history",
                    f.label
                ));
            }
            if let Some(p) = &f.placeholder {
                too_long("field placeholder", p, 100)?;
            }
            if let Some(v) = &f.value {
                too_long("field value", v, 4000)?;
            }
            fields.push(FormField {
                id: format!("f{}", i + 1),
                label: f.label,
                style: if f.multiline {
                    FieldStyle::Paragraph
                } else {
                    FieldStyle::Short
                },
                required: f.required,
                placeholder: f.placeholder,
                min_len: None,
                max_len: Some(if f.multiline { 4000 } else { 1000 }),
                value: f.value,
            });
        }
        Ok(Self {
            button,
            form: Form {
                title: args.title,
                fields,
            },
        })
    }
}

/// A view made for one channel: messages sent before it, then the prompt (or nothing more when
/// the platform shows none of it natively: then everything is in `lead`).
struct Built {
    lead: Vec<okbot_core::RenderedMessage>,
    prompt: Option<Prompt>,
}

fn short_id() -> String {
    format!("v{}", hex::encode(rand::random::<[u8; 4]>()))
}

/// What of a view a platform shows natively.
#[derive(Clone, Copy)]
struct Native {
    cards: bool,
    buttons: bool,
    select: bool,
    form: bool,
    links: bool,
}

impl Native {
    fn of(view: &View, ui: okbot_core::UiCaps) -> Self {
        let links = ui.buttons && !view.links.is_empty();
        let form = ui.buttons && ui.forms && view.form.is_some();
        let n = view.choices.len();
        let extra = if links { view.links.len() } else { 0 } + usize::from(form);
        let buttons = ui.buttons && n > 0 && n + extra <= okbot_core::MAX_BUTTONS;
        Self {
            cards: ui.cards && !view.cards.is_empty(),
            buttons,
            select: !buttons && ui.select && n >= 2,
            form,
            links,
        }
    }

    fn any(self) -> bool {
        self.cards || self.buttons || self.select || self.form || self.links
    }
}

/// Makes `view` for a channel with `caps`: what the platform shows natively goes into a prompt,
/// the rest into its text. `image` is a card's chart drawn as a picture, by card index.
fn build(
    renderer: &dyn okbot_core::Renderer,
    view: &View,
    caps: &ChannelCaps,
    image: Option<(usize, Image)>,
) -> Built {
    let native = Native::of(view, caps.ui);
    let mut text = vec![view.text.clone()];
    if !native.cards {
        text.extend(view.cards.iter().map(View::card_markdown));
    }
    if !native.buttons && !native.select {
        text.push(view.choices_markdown());
    }
    if !native.form {
        text.extend(view.form.iter().map(View::form_markdown));
    }
    if !native.links {
        text.push(view.links_markdown());
    }
    let markdown = join(text);
    let mut lead = if markdown.is_empty() {
        Vec::new()
    } else {
        renderer.render(&markdown, caps)
    };
    if !native.any() {
        return Built { lead, prompt: None };
    }
    // Long text goes first; the prompt carries its last part.
    let mut text = lead.pop().map(|m| m.text).unwrap_or_default();
    if text.is_empty() && !native.cards {
        // Some platforms need text with their buttons.
        text = view.placeholder.clone().unwrap_or_else(|| "👇".into());
    }
    let mut prompt = Prompt {
        id: short_id(),
        text,
        ..Prompt::default()
    };
    if native.cards {
        let mut image = image;
        prompt.cards = view
            .cards
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let image = image.take_if(|(at, _)| *at == i).map(|(_, image)| image);
                let body = c.body(image.is_none());
                Card {
                    title: c.title.clone(),
                    tone: c.tone,
                    text: renderer
                        .render(&body, caps)
                        .into_iter()
                        .map(|m| m.text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    image,
                }
            })
            .collect();
    }
    actions(&mut prompt, view, native);
    Built {
        lead,
        prompt: Some(prompt),
    }
}

/// Adds the view's choices (as buttons or a menu), form and links to `prompt`, as far as the
/// platform shows them.
fn actions(prompt: &mut Prompt, view: &View, native: Native) {
    if native.buttons {
        prompt
            .buttons
            .extend(view.choices.iter().enumerate().map(|(i, c)| PromptButton {
                id: format!("b{}", i + 1),
                label: c.label.clone(),
                style: c.style,
                form: None,
                url: None,
            }));
    }
    if native.select {
        prompt.select = Some(Select {
            placeholder: view.placeholder.clone().unwrap_or_else(|| "…".into()),
            options: view
                .choices
                .iter()
                .enumerate()
                .map(|(i, c)| SelectOption {
                    id: format!("o{}", i + 1),
                    label: c.label.clone(),
                    description: c.description.clone(),
                })
                .collect(),
        });
    }
    if let Some(f) = view.form.as_ref().filter(|_| native.form) {
        prompt.buttons.push(PromptButton {
            id: FORM.into(),
            label: f.button.clone(),
            style: ButtonStyle::Primary,
            form: Some(f.form.clone()),
            url: None,
        });
    }
    if native.links {
        prompt
            .buttons
            .extend(view.links.iter().enumerate().map(|(i, l)| PromptButton {
                id: format!("l{}", i + 1),
                label: l.label.clone(),
                style: ButtonStyle::Secondary,
                form: None,
                url: Some(l.url.clone()),
            }));
    }
}

/// A view waiting for an answer, as kept in the store.
#[derive(Debug, Serialize, Deserialize)]
struct State {
    /// The prompt as shown (adapters that restarted get it back).
    prompt: Prompt,
    /// What each answer id says: a choice's label.
    labels: BTreeMap<String, String>,
    /// The form behind the form button.
    form: Option<Form>,
    conversation: ConversationKey,
    is_direct: bool,
    sent: SentRef,
}

/// The views a turn's agent asked to show (none when the room is not interactive).
pub(crate) fn take(shown: Option<Shown>) -> Vec<View> {
    shown
        .map(|v| std::mem::take(&mut *lock(&v)))
        .unwrap_or_default()
}

/// Sends the views of a turn after its answer. Failures fall back to text, then to a log line.
pub(crate) async fn send(shared: &Shared, event: &InboundEvent, views: &[View]) {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        return;
    };
    let to = crate::dispatch::target(event);
    for view in views {
        let image = chart_image(shared, view, channel.caps()).await;
        let built = build(shared.renderer.as_ref(), view, channel.caps(), image);
        if let Err(error) = show(shared, event, &to, view, built).await {
            tracing::warn!(%error, "an interactive view failed; sending it as text");
            if let Err(error) = crate::turn::send_to(shared, &to, &view.markdown()).await {
                tracing::error!(%error, "cannot send a view as text");
            }
        }
    }
}

/// The first card chart of `view` drawn as a picture, when the bot draws charts and the channel
/// shows cards with pictures. A chart that cannot be drawn in time stays text.
async fn chart_image(shared: &Shared, view: &View, caps: &ChannelCaps) -> Option<(usize, Image)> {
    let charts = shared.charts.clone()?;
    if !(caps.ui.cards && caps.ui.images) {
        return None;
    }
    let (at, chart) = view
        .cards
        .iter()
        .enumerate()
        .find_map(|(i, c)| c.chart.clone().map(|chart| (i, chart)))?;
    let drawing = tokio::task::spawn_blocking(move || charts.render(&chart));
    match tokio::time::timeout(CHART_TIMEOUT, drawing).await {
        Ok(Ok(Ok(image))) => Some((at, image)),
        Ok(Ok(Err(error))) => {
            tracing::info!(%error, "a chart is shown as text");
            None
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "drawing a chart failed");
            None
        }
        Err(_) => {
            tracing::warn!("drawing a chart took too long; it is shown as text");
            None
        }
    }
}

async fn show(
    shared: &Shared,
    event: &InboundEvent,
    to: &ReplyTarget,
    view: &View,
    built: Built,
) -> Result<(), okbot_core::ChannelError> {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        return Ok(());
    };
    for part in &built.lead {
        channel.send(to, part).await?;
    }
    let Some(prompt) = built.prompt else {
        return Ok(());
    };
    let sent = channel.prompt(to, &prompt).await?;
    if !prompt.is_answerable() {
        return Ok(());
    }
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    for (i, c) in view.choices.iter().enumerate() {
        labels.insert(format!("b{}", i + 1), c.label.clone());
        labels.insert(format!("o{}", i + 1), c.label.clone());
    }
    let state = State {
        form: view.form.as_ref().map(|f| f.form.clone()),
        prompt,
        labels,
        conversation: event.conversation.clone(),
        is_direct: event.is_direct,
        sent,
    };
    let expires = shared.clock.now()
        + jiff::SignedDuration::try_from(TTL).unwrap_or(jiff::SignedDuration::MAX);
    let data =
        serde_json::to_string(&state).map_err(|e| okbot_core::ChannelError::Failed(e.into()))?;
    if let Err(error) = shared.store.put_view(&state.prompt.id, data, expires).await {
        // Shown but not answerable: its presses find nothing. Typing still works.
        tracing::error!(%error, "cannot keep an interactive view");
    }
    tracing::info!(prompt = %state.prompt.id, "interactive view shown");
    Ok(())
}

/// The prompt of a view still waiting, for a channel that restarted since it showed it.
pub(crate) async fn lookup(shared: &Shared, id: &str) -> Option<Prompt> {
    let data = shared
        .store
        .view(id, shared.clock.now())
        .await
        .ok()
        .flatten()?;
    serde_json::from_str::<State>(&data).ok().map(|s| s.prompt)
}

/// The clicker's next message: `→ ` and the choice, or the form's title and values. The arrow
/// also keeps a label from being read as a `/command`.
fn answer_text(state: &State, answer: &PromptAnswer) -> Option<String> {
    if answer.button == FORM {
        let form = state.form.as_ref()?;
        let mut text = format!("→ {}", form.title);
        for f in &form.fields {
            let value = answer
                .values
                .get(&f.id)
                .map(|v| v.expose().trim().to_string());
            let value = value
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "—".into());
            let _ = write!(text, "\n{}: {value}", f.label);
        }
        return Some(text);
    }
    state
        .labels
        .get(&answer.button)
        .map(|label| format!("→ {label}"))
}

/// An answer to a view: the first one closes it (showing who chose what) and goes on as the
/// clicker's message. Presses by people the bot would not answer here are ignored.
pub(crate) async fn answer(shared: Arc<Shared>, answer: PromptAnswer) {
    let now = shared.clock.now();
    let Ok(Some(data)) = shared.store.view(&answer.prompt, now).await else {
        tracing::debug!(prompt = %answer.prompt, "answer to an unknown or expired view");
        return;
    };
    let Ok(state) = serde_json::from_str::<State>(&data) else {
        return;
    };
    if state.conversation.channel != answer.conversation.channel
        || state.conversation.room != answer.conversation.room
        || !state.prompt.answers_with(&answer.button)
    {
        return;
    }
    let Some(text) = answer_text(&state, &answer) else {
        return;
    };
    let event = InboundEvent {
        event_id: format!("ui:{}:{}", answer.prompt, answer.user.id),
        conversation: state.conversation.clone(),
        author: answer.user.clone(),
        text,
        attachments: Vec::new(),
        // A press is addressed to the bot.
        mentions_bot: true,
        is_direct: state.is_direct,
        restricted: false,
        message_id: None,
        sent_at: now,
        locale: None,
    };
    if !shared.gate.admits(&event) {
        tracing::info!(prompt = %answer.prompt, user = %answer.user.id, "view answer ignored: not answered here");
        return;
    }
    // The first answer wins; a press racing it finds nothing.
    if !matches!(
        shared.store.take_view(&answer.prompt, now).await,
        Ok(Some(_))
    ) {
        return;
    }
    let chosen = if answer.button == FORM {
        state
            .form
            .as_ref()
            .map(|f| f.title.clone())
            .unwrap_or_default()
    } else {
        state
            .labels
            .get(&answer.button)
            .cloned()
            .unwrap_or_default()
    };
    close(
        &shared,
        &state,
        &format!("→ ✅ {}: {chosen}", answer.user.display_name),
    )
    .await;
    tracing::info!(prompt = %answer.prompt, user = %answer.user.id, "view answered");
    let Some(sink) = shared.sink.get().and_then(std::sync::Weak::upgrade) else {
        return;
    };
    if sink.deliver(event).await.is_err() {
        tracing::warn!("the runtime stopped before a view answer was handled");
    }
}

/// Shows the outcome under a view and removes what could be answered (links stay).
async fn close(shared: &Shared, state: &State, outcome: &str) {
    let Some(channel) = shared.channels.get(&state.sent.target.conversation.channel) else {
        return;
    };
    let outcome: Vec<String> = shared
        .renderer
        .render(outcome, channel.caps())
        .into_iter()
        .map(|m| m.text)
        .collect();
    let mut prompt = state.prompt.clone();
    prompt.text = join(vec![prompt.text, outcome.join("\n")]);
    prompt.select = None;
    prompt.buttons.retain(|b| b.url.is_some());
    if let Err(error) = channel.update_prompt(&state.sent, &prompt).await {
        tracing::warn!(%error, "cannot close a view");
    }
}

/// The `chart` of a card in the `show` schema (apart: one `json!` would be too deep).
fn chart_schema() -> Value {
    json!({"type": "object", "description": CHART_DESCRIPTION, "properties": {
                            "kind": {"type": "string", "enum": ["bar", "column", "line"]},
                            "unit": {"type": "string"},
                            "points": {"type": "array", "description": "One series.", "items": {"type": "object",
                                "properties": {"label": {"type": "string"}, "value": {"type": "number"}},
                                "required": ["label", "value"]}},
                            "labels": {"type": "array", "items": {"type": "string"},
                                "description": "With series, instead of points."},
                            "series": {"type": "array", "maxItems": okbot_core::MAX_SERIES, "items": {
                                "type": "object", "properties": {
                                    "name": {"type": "string"},
                                    "values": {"type": "array", "items": {"type": ["number", "null"]}}},
                                "required": ["values"]}},
                            "highlight": {"type": "string", "description": "The label to stand out."},
                            "locale": {"type": "string", "description": "vi, en or ja, for numbers."}}})
}

fn tools() -> Value {
    json!([{
        "name": "show",
        "description": TOOL_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "Optional short line above the cards (Markdown)."},
                "cards": {"type": "array", "maxItems": okbot_core::MAX_CARDS, "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "tone": {"type": "string", "enum": ["neutral", "info", "success", "warning", "error"]},
                        "text": {"type": "string", "description": "Markdown."},
                        "fields": {"type": "array", "items": {"type": "object",
                            "properties": {"label": {"type": "string"}, "value": {"type": "string"}},
                            "required": ["label", "value"]}},
                        "table": {"type": "object", "properties": {
                            "columns": {"type": "array", "items": {"type": "string"}},
                            "rows": {"type": "array", "items": {"type": "array"}}},
                            "required": ["rows"]},
                        "chart": chart_schema()
                    }
                }},
                "choices": {"type": "array", "minItems": 2, "maxItems": MAX_CHOICES,
                    "description": "Options the reader picks one of: up to 5 become buttons, more a menu.",
                    "items": {"anyOf": [
                        {"type": "string"},
                        {"type": "object", "properties": {
                            "label": {"type": "string"},
                            "description": {"type": "string"},
                            "style": {"type": "string", "enum": ["primary", "secondary", "success", "danger"]}},
                         "required": ["label"]}
                    ]}},
                "placeholder": {"type": "string", "description": "Shown on a menu before a pick."},
                "form": {"type": "object", "description": "A button that opens a form.", "properties": {
                    "button": {"type": "string"},
                    "title": {"type": "string"},
                    "fields": {"type": "array", "minItems": 1, "maxItems": okbot_core::MAX_FIELDS, "items": {
                        "type": "object", "properties": {
                            "label": {"type": "string"},
                            "multiline": {"type": "boolean"},
                            "required": {"type": "boolean"},
                            "placeholder": {"type": "string"},
                            "value": {"type": "string", "description": "Pre-filled value."}},
                        "required": ["label"]}}},
                    "required": ["title", "fields"]},
                "links": {"type": "array", "maxItems": MAX_LINKS, "items": {"type": "object",
                    "properties": {"label": {"type": "string"}, "url": {"type": "string"}},
                    "required": ["label", "url"]}}
            }
        },
        "annotations": {"readOnlyHint": true}
    }])
}

const TOOL_DESCRIPTION: &str = "Show cards, buttons, a menu, a form or link buttons after your \
answer, natively on the chat platform (as text where it has none). Your answer text still \
answers; call this only when it helps the reader act faster than text: data with structure \
(fields, a table, a chart) as cards; a decision as 2-25 choices (labels short, in the user's \
language); several values to type as a form (at most 5 fields, never passwords, codes, tokens \
or card numbers). The person's pick or form arrives as their next message, starting with `→`. \
Typing still works, so do not write \"reply YES to…\" when there are choices. Buttons are not an \
approval: tools that change real data ask for it themselves. At most 3 calls per answer.";

const CHART_DESCRIPTION: &str = "A chart: `bar` (default; comparing or ranking, up to 15 \
labels, one series), `column` (short ordered categories like months, up to 12 labels, 1-3 \
series) or `line` (a trend, 2-60 points, 1-3 series). Give `points`, or `labels` with `series` \
(one value per label, null for a gap). Labels in the user's language.";

/// The `<interactive>` section of the system prompt.
pub(crate) const PROMPT_NOTE: &str = "Interactive replies are on. Besides your answer, you can \
show cards (data with structure: fields, tables, charts), 2-25 choices (up to 5 become buttons, \
more a menu), a form (up to 5 fields) and link buttons with the `show` tool of the `ui` server. \
Use it when the reader will choose or fill in something, or read structured data; otherwise \
plain text is better. Keep the answer itself in your text. Choice labels are short and in the \
user's language. Never ask for passwords, codes, tokens or card numbers. A pick or form arrives \
as the user's next message, starting with `→`.";

/// The built-in `ui` server (JSON-RPC over the gateway, like `team`).
pub(crate) async fn post(shared: &Shared, grant: &Grant, shown: &Shown, body: &[u8]) -> Response {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return crate::team::json_response(&json!({"jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "parse error"}}));
    };
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    match message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "initialize" => crate::team::rpc_result(
            &id,
            &json!({
                "protocolVersion": message.pointer("/params/protocolVersion")
                    .and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "okbot-ui", "version": env!("CARGO_PKG_VERSION")},
                "instructions": PROMPT_NOTE,
            }),
        ),
        "ping" => crate::team::rpc_result(&id, &json!({})),
        "tools/list" => crate::team::rpc_result(&id, &json!({"tools": tools()})),
        "tools/call" => {
            let tool = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = message
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(Value::Null);
            let started = Instant::now();
            let result = if tool == "show" {
                call_show(shared, grant, shown, args)
            } else {
                Err(format!("no tool {tool:?}"))
            };
            let (text, error) = match result {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            crate::gateway::audit(
                shared,
                ToolCallRecord {
                    run_id: Some(grant.run_id.clone()),
                    command: None,
                    person: grant.person.clone(),
                    conversation: grant.event.conversation.clone(),
                    server: SERVER.into(),
                    tool,
                    decision: ToolDecision::Allowed,
                    error: error.then(|| text.chars().take(200).collect()),
                    duration: started.elapsed(),
                    at: shared.clock.now(),
                },
            )
            .await;
            crate::team::tool_text(&id, &text, error)
        }
        _ => crate::team::json_response(&json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": "not offered through okbot"}})),
    }
}

fn call_show(shared: &Shared, grant: &Grant, shown: &Shown, args: Value) -> Result<String, String> {
    let args: ShowArgs =
        serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
    let view = View::from_args(args)?;
    // Checked against this platform now, so the agent can still fix it.
    if let Some(channel) = shared.channels.get(&grant.event.conversation.channel)
        && let Some(prompt) = build(shared.renderer.as_ref(), &view, channel.caps(), None).prompt
    {
        prompt.validate()?;
    }
    let mut views = lock(shown);
    if views.len() >= MAX_VIEWS {
        return Err(format!("at most {MAX_VIEWS} views per answer"));
    }
    views.push(view);
    Ok("It will be shown after your answer text.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{LengthUnit, MarkdownDialect, UiCaps};

    fn args(value: Value) -> Result<View, String> {
        View::from_args(serde_json::from_value(value).map_err(|e| e.to_string())?)
    }

    fn caps(ui: UiCaps) -> ChannelCaps {
        ChannelCaps {
            max_message_len: 2000,
            length_unit: LengthUnit::Chars,
            dialect: MarkdownDialect::DiscordMd,
            threads: true,
            edit: true,
            typing: true,
            ui,
        }
    }

    fn renderer() -> okbot_testkit::PlainRenderer {
        okbot_testkit::PlainRenderer
    }

    #[test]
    fn checks_what_the_agent_asks_for() {
        assert!(args(json!({})).is_err(), "nothing to show");
        assert!(args(json!({"choices": ["Only"]})).is_err());
        assert!(args(json!({"choices": ["A", "A"]})).is_err());
        assert!(args(json!({"links": [{"label": "x", "url": "http://a"}]})).is_err());
        let secret = args(json!({"form": {"title": "Login", "fields": [{"label": "Mật khẩu"}]}}));
        assert!(secret.unwrap_err().contains("never ask"));
        assert!(
            args(json!({"form": {"title": "Card", "fields": [{"label": "OTP code"}]}})).is_err()
        );
        assert!(
            args(json!({"form": {"title": "Trip", "fields": [{"label": "Departure point"}]}}))
                .is_ok()
        );
        assert!(
            args(json!({"bogus": 1})).is_err(),
            "unknown fields are refused"
        );
    }

    #[test]
    fn tables_and_charts_become_chat_lines() {
        let view = args(json!({"cards": [{
            "title": "Stock", "tone": "warning",
            "table": {"columns": ["Item", "Qty"], "rows": [["Tea", 3], ["Rice", 0]]},
            "chart": {"unit": "kg", "points": [{"label": "Tea", "value": 3}, {"label": "Rice", "value": 1.5}]}
        }]}))
        .unwrap();
        insta::assert_snapshot!(view.markdown(), @r"
        **⚠️ Stock**
        -# Item · Qty
        • **Tea** · 3
        • **Rice** · 0
        Tea `▇▇▇▇▇▇▇▇▇▇▇▇` 3 kg
        Rice `▇▇▇▇▇▇` 1.5 kg
        ");
    }

    #[test]
    fn charts_with_several_series_and_their_mistakes() {
        let view = args(json!({"cards": [{"title": "Sales", "chart": {
            "kind": "column", "unit": "k",
            "labels": ["Jan", "Feb"],
            "series": [{"name": "2025", "values": [3, null]}, {"name": "2026", "values": [4, 5]}]
        }}]}))
        .unwrap();
        insta::assert_snapshot!(view.markdown(), @r"
        **Sales**
        -# 2025 · 2026 k
        • **Jan** · 3 · 4
        • **Feb** · — · 5
        ");
        let wrong = [
            (
                json!({"points": [{"label": "a", "value": 1}], "labels": ["a"]}),
                "either points",
            ),
            (
                json!({"points": [{"label": "a", "value": 1}], "highlight": "b"}),
                "highlight",
            ),
            (
                json!({"kind": "line", "points": [{"label": "a", "value": 1}]}),
                "2 to 60",
            ),
            (
                json!({"labels": ["a"], "series": [{"values": [1]}, {"values": [2]}]}),
                "1 to 1",
            ),
            (
                json!({"kind": "column", "labels": ["a"],
                "series": [{"values": [1]}, {"values": [2]}]}),
                "names",
            ),
        ];
        for (chart, error) in wrong {
            let result = args(json!({"cards": [{"chart": chart}]}));
            assert!(
                result.as_ref().is_err_and(|e| e.contains(error)),
                "{result:?}"
            );
        }
    }

    #[test]
    fn a_chart_picture_replaces_its_text_in_the_card() {
        let view = args(json!({"cards": [
            {"title": "Note", "text": "hello"},
            {"title": "Stock", "text": "This week", "chart": {"points": [{"label": "Tea", "value": 3}]}}
        ]}))
        .unwrap();
        let image = Image {
            name: "chart.png".into(),
            alt: "Bar chart".into(),
            width: 1200,
            height: 300,
            png: vec![1, 2, 3],
        };
        let prompt = build(
            &renderer(),
            &view,
            &caps(UiCaps::all()),
            Some((1, image.clone())),
        )
        .prompt
        .unwrap();
        assert_eq!(prompt.cards[0].image, None);
        assert_eq!(prompt.cards[1].image, Some(image));
        assert_eq!(prompt.cards[1].text, "This week");
        // Without the picture the chart is text, and the history always keeps it as text.
        let prompt = build(&renderer(), &view, &caps(UiCaps::all()), None)
            .prompt
            .unwrap();
        assert!(
            prompt.cards[1].text.contains("Tea `▇"),
            "{}",
            prompt.cards[1].text
        );
        assert!(view.markdown().contains("Tea `▇"));
    }

    #[test]
    fn builds_what_the_platform_shows_and_text_for_the_rest() {
        let view = args(json!({
            "text": "Pick a slot",
            "cards": [{"title": "Today", "text": "Two slots left"}],
            "choices": ["9:00", {"label": "10:00", "style": "success"}],
            "form": {"button": "Other time", "title": "Other time", "fields": [{"label": "When"}]},
            "links": [{"label": "Calendar", "url": "https://example.com/cal"}]
        }))
        .unwrap();
        let all = build(&renderer(), &view, &caps(UiCaps::all()), None);
        let prompt = all.prompt.expect("a prompt");
        assert_eq!(all.lead, []);
        assert_eq!(prompt.text, "Pick a slot");
        assert_eq!(prompt.cards.len(), 1);
        let ids: Vec<&str> = prompt.buttons.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2", "form", "l1"]);
        assert!(prompt.validate().is_ok());

        let text = build(&renderer(), &view, &caps(UiCaps::default()), None);
        assert!(text.prompt.is_none());
        let all_text: Vec<&str> = text.lead.iter().map(|m| m.text.as_str()).collect();
        let all_text = all_text.join("\n");
        assert!(
            all_text.contains("1. 9:00") && all_text.contains("When"),
            "{all_text}"
        );
        assert!(all_text.contains("https://example.com/cal"));

        // Buttons without cards or forms (Telegram): cards and the form become text.
        let telegram = UiCaps {
            buttons: true,
            select: true,
            ..UiCaps::default()
        };
        let built = build(&renderer(), &view, &caps(telegram), None);
        let prompt = built.prompt.expect("a prompt");
        assert_eq!(prompt.cards, []);
        assert!(prompt.text.contains("Today") && prompt.text.contains("When"));
        let ids: Vec<&str> = prompt.buttons.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2", "l1"]);
    }

    #[test]
    fn many_choices_make_a_menu() {
        let choices: Vec<String> = (1..=8).map(|i| format!("Item {i}")).collect();
        let view = args(json!({"choices": choices, "placeholder": "Pick one"})).unwrap();
        let prompt = build(&renderer(), &view, &caps(UiCaps::all()), None)
            .prompt
            .expect("a prompt");
        let select = prompt.select.as_ref().expect("a menu");
        assert_eq!(select.options.len(), 8);
        assert_eq!(select.options[7].id, "o8");
        assert_eq!(prompt.buttons, []);
        assert!(prompt.validate().is_ok());
    }

    #[test]
    fn answers_become_messages() {
        let view = args(json!({
            "choices": ["Yes", "/reset"],
            "form": {"title": "Address", "fields": [{"label": "Street"}, {"label": "Note", "required": false}]}
        }))
        .unwrap();
        let prompt = build(&renderer(), &view, &caps(UiCaps::all()), None)
            .prompt
            .expect("a prompt");
        let key = okbot_testkit::conversation("discord:main", "c1");
        let state = State {
            labels: BTreeMap::from([("b2".into(), "/reset".into())]),
            form: view.form.as_ref().map(|f| f.form.clone()),
            prompt,
            conversation: key.clone(),
            is_direct: false,
            sent: SentRef {
                target: ReplyTarget {
                    conversation: key.clone(),
                    in_reply_to: None,
                },
                message_id: okbot_core::MessageId::new("m1"),
            },
        };
        let mut answer = PromptAnswer {
            prompt: "v1".into(),
            button: "b2".into(),
            values: BTreeMap::new(),
            user: okbot_core::UserRef {
                id: okbot_core::UserId::new("u1"),
                display_name: "An".into(),
                is_bot: false,
            },
            conversation: key,
            sent: state.sent.clone(),
        };
        assert_eq!(answer_text(&state, &answer).as_deref(), Some("→ /reset"));
        answer.button = FORM.into();
        answer
            .values
            .insert("f1".into(), okbot_core::Secret::new("1 Main St".into()));
        assert_eq!(
            answer_text(&state, &answer).as_deref(),
            Some("→ Address\nStreet: 1 Main St\nNote: —")
        );
    }
}

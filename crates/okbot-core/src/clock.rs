//! Time source.

use jiff::Timestamp;

/// A source of the current time, replaceable in tests.
pub trait Clock: Send + Sync {
    /// Now, in UTC.
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

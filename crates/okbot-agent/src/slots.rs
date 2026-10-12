//! A machine-wide limit on running agent processes, shared by every bot on the machine.
//!
//! Each slot is a lock file in a shared directory; a turn holds an exclusive lock on one of them.
//! Locks are released by the operating system when the process exits, so a crashed bot never
//! leaks a slot.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::time::Duration;

/// `max` slots in `dir`.
#[derive(Debug, Clone)]
pub struct MachineSlots {
    dir: PathBuf,
    max: usize,
}

/// A held slot; dropping it frees the slot.
#[derive(Debug)]
pub struct Slot(#[allow(dead_code)] File);

impl MachineSlots {
    /// Slots in `dir` (created if missing), at most `max` agents at once.
    pub fn new(dir: impl Into<PathBuf>, max: usize) -> Self {
        Self {
            dir: dir.into(),
            max: max.max(1),
        }
    }

    fn try_acquire(&self) -> std::io::Result<Option<Slot>> {
        std::fs::create_dir_all(&self.dir)?;
        for i in 0..self.max {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.dir.join(format!("slot-{i}.lock")))?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(Slot(file))),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Waits for a free slot.
    pub async fn acquire(&self) -> std::io::Result<Slot> {
        loop {
            let this = self.clone();
            let got = tokio::task::spawn_blocking(move || this.try_acquire())
                .await
                .map_err(std::io::Error::other)??;
            if let Some(slot) = got {
                return Ok(slot);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limits_holders_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let a = MachineSlots::new(dir.path(), 2);
        let b = MachineSlots::new(dir.path(), 2); // another bot, same machine
        let s1 = a.acquire().await.unwrap();
        let _s2 = b.acquire().await.unwrap();
        assert!(a.try_acquire().unwrap().is_none(), "both slots taken");
        drop(s1);
        assert!(
            b.try_acquire().unwrap().is_some(),
            "a freed slot is reusable"
        );
    }
}

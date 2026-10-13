//! An in-memory [`TaskStore`].

use std::sync::{Arc, Mutex};

use okbot_core::{
    Clock, NewTask, Requester, Task, TaskChange, TaskError, TaskStatus, TaskStore, async_trait,
    sort_tasks,
};

/// A [`TaskStore`] kept in memory, for runtime tests: every requester sees every task, ids are
/// `tasks/test/t-<n>`, and no rule is checked.
pub struct MemoryTasks {
    tasks: Mutex<Vec<Task>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for MemoryTasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryTasks").finish_non_exhaustive()
    }
}

impl MemoryTasks {
    /// An empty store reading time from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Task>> {
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every task, in display order.
    pub fn all(&self) -> Vec<Task> {
        let mut tasks = self.lock().clone();
        sort_tasks(&mut tasks);
        tasks
    }

    /// Adds a fully built task as it is (for tests that need a past time or an origin).
    pub fn insert(&self, task: Task) {
        self.lock().push(task);
    }

    fn change(&self, id: &str, change: TaskChange) -> Result<Task, TaskError> {
        let mut tasks = self.lock();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id || t.id.rsplit('/').next() == Some(id))
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?;
        if let Some(title) = change.title {
            task.title = title;
        }
        if let Some(notes) = change.notes {
            task.notes = notes;
        }
        if let Some(status) = change.status {
            task.status = status;
        }
        if let Some(due) = change.due {
            task.due = due;
        }
        if let Some(at) = change.remind_at {
            task.remind_at = at;
        }
        if let Some(repeat) = change.repeat {
            task.repeat = repeat;
        }
        Ok(task.clone())
    }
}

#[async_trait]
impl TaskStore for MemoryTasks {
    async fn add(&self, who: &Requester, new: NewTask) -> Result<Task, TaskError> {
        let mut tasks = self.lock();
        let task = Task {
            id: format!("tasks/test/t-{}", tasks.len() + 1),
            kind: new.kind,
            status: if new.paused {
                TaskStatus::Paused
            } else {
                TaskStatus::Open
            },
            title: new.title,
            notes: new.notes,
            scope: new.scope,
            due: new.due,
            remind_at: new.remind_at,
            repeat: new.repeat,
            deliver_to: new.deliver_to,
            origin: Some(who.conversation.clone()),
            author: format!("{}/{}", who.conversation.channel, who.user.id),
            created: self.clock.now(),
        };
        tasks.push(task.clone());
        Ok(task)
    }

    async fn list(&self, _who: &Requester) -> Result<Vec<Task>, TaskError> {
        Ok(self.all())
    }

    async fn update(
        &self,
        _who: &Requester,
        id: &str,
        change: TaskChange,
    ) -> Result<Task, TaskError> {
        self.change(id, change)
    }

    async fn delete(&self, _who: &Requester, id: &str) -> Result<Task, TaskError> {
        let mut tasks = self.lock();
        let at = tasks
            .iter()
            .position(|t| t.id == id || t.id.rsplit('/').next() == Some(id))
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?;
        Ok(tasks.remove(at))
    }

    async fn scheduled(&self) -> Result<Vec<Task>, TaskError> {
        Ok(self
            .all()
            .into_iter()
            .filter(|t| t.status.is_active() && t.remind_at.is_some())
            .collect())
    }

    async fn after_fire(&self, id: &str, change: TaskChange) -> Result<Task, TaskError> {
        self.change(id, change)
    }
}

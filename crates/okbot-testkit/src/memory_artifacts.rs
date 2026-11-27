//! An in-memory [`ArtifactStore`].

use std::sync::{Arc, Mutex};

use okbot_core::{
    Artifact, ArtifactError, ArtifactStore, ArtifactVersion, Clock, NewArtifact, Requester,
    async_trait,
};

/// An [`ArtifactStore`] kept in memory, for runtime tests: every requester sees every artifact,
/// ids are `artifacts/test/a-<n>`, and no limit is checked.
pub struct MemoryArtifacts {
    items: Mutex<Vec<(Artifact, Vec<String>)>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for MemoryArtifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryArtifacts").finish_non_exhaustive()
    }
}

impl MemoryArtifacts {
    /// An empty store reading time from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Artifact, Vec<String>)>> {
        self.items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every artifact, oldest first.
    pub fn all(&self) -> Vec<Artifact> {
        self.lock().iter().map(|(a, _)| a.clone()).collect()
    }
}

fn matches(a: &Artifact, id: &str) -> bool {
    a.id == id || a.short_id() == id
}

#[async_trait]
impl ArtifactStore for MemoryArtifacts {
    async fn save(&self, who: &Requester, new: NewArtifact) -> Result<Artifact, ArtifactError> {
        let now = self.clock.now();
        let by = format!("{}/{}", who.conversation.channel, who.user.id);
        let mut items = self.lock();
        let version = |n: u32| ArtifactVersion {
            n,
            at: now,
            by: by.clone(),
            bytes: new.content.len() as u64,
            sha256: String::new(),
            note: new.note.clone(),
            pruned: false,
        };
        if let Some(id) = &new.id {
            let (a, contents) = items
                .iter_mut()
                .find(|(a, _)| matches(a, id))
                .ok_or_else(|| ArtifactError::NotFound(id.clone()))?;
            let n = a.latest().n + 1;
            a.versions.push(version(n));
            if !new.title.is_empty() {
                a.title.clone_from(&new.title);
            }
            if !new.summary.is_empty() {
                a.summary.clone_from(&new.summary);
            }
            contents.push(new.content);
            return Ok(a.clone());
        }
        let artifact = Artifact {
            id: format!("artifacts/test/a-{}", items.len() + 1),
            title: new.title.clone(),
            kind: new.kind,
            summary: new.summary.clone(),
            scope: new.scope,
            author: by.clone(),
            origin: Some(who.conversation.clone()),
            created: now,
            versions: vec![version(1)],
        };
        items.push((artifact.clone(), vec![new.content]));
        Ok(artifact)
    }

    async fn list(&self, _who: &Requester) -> Result<Vec<Artifact>, ArtifactError> {
        let mut all = self.all();
        all.sort_by_key(|a| std::cmp::Reverse(a.updated()));
        Ok(all)
    }

    async fn open(
        &self,
        _who: &Requester,
        id: &str,
        version: Option<u32>,
    ) -> Result<(Artifact, u32, String), ArtifactError> {
        let items = self.lock();
        let (a, contents) = items
            .iter()
            .find(|(a, _)| matches(a, id))
            .ok_or_else(|| ArtifactError::NotFound(id.to_string()))?;
        let n = version.unwrap_or(a.latest().n);
        let content = usize::try_from(n)
            .ok()
            .and_then(|n| contents.get(n.checked_sub(1)?))
            .ok_or_else(|| ArtifactError::NotFound(format!("{id} v{n}")))?;
        Ok((a.clone(), n, content.clone()))
    }

    async fn rename(
        &self,
        _who: &Requester,
        id: &str,
        title: &str,
    ) -> Result<Artifact, ArtifactError> {
        let mut items = self.lock();
        let (a, _) = items
            .iter_mut()
            .find(|(a, _)| matches(a, id))
            .ok_or_else(|| ArtifactError::NotFound(id.to_string()))?;
        a.title = title.to_string();
        Ok(a.clone())
    }

    async fn delete(&self, _who: &Requester, id: &str) -> Result<Artifact, ArtifactError> {
        let mut items = self.lock();
        let i = items
            .iter()
            .position(|(a, _)| matches(a, id))
            .ok_or_else(|| ArtifactError::NotFound(id.to_string()))?;
        Ok(items.remove(i).0)
    }
}

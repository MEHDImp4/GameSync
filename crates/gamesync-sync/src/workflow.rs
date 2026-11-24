use crate::{ApplyError, Restore, ScanError, capture, scan};
use gamesync_core::{Decision, Manifest, decide};
use gamesync_protocol::{ClientError, Session};
use gamesync_storage::{ObjectError, ObjectStore};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Synchronized(i64),
    Conflict { variant: i64 },
    Deferred,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Api(#[from] ClientError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Restore(#[from] ApplyError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the configured game or device changed")]
    Identity,
    #[error("server lost the previously recorded head")]
    MissingHead,
    #[error("another sync is already running")]
    Busy,
    #[error("the server head changed; refresh the history before choosing a revision")]
    StaleSelection,
    #[error("restore the previous folder mapping to finish the pending synchronization")]
    PendingRemap,
}

#[derive(Serialize, Deserialize)]
struct PendingPublish {
    request: Uuid,
    base: Option<i64>,
    manifest: Manifest,
    #[serde(default)]
    replace_local: Option<Manifest>,
    #[serde(default)]
    resolves: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct PendingApply {
    revision: i64,
    local: Manifest,
    target: Manifest,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    base: Option<i64>,
    #[serde(default)]
    base_manifest: Option<Manifest>,
    #[serde(default)]
    remapped: bool,
    conflict: Option<i64>,
    publish: Option<PendingPublish>,
    apply: Option<PendingApply>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    game: Uuid,
    device: Uuid,
    state: State,
    #[serde(default)]
    roots: Option<Vec<(Uuid, PathBuf)>>,
}

/// Coordinates one game. Use a private data directory outside every save root.
pub struct Workflow<'a> {
    pub game: Uuid,
    pub roots: &'a [(Uuid, PathBuf)],
    pub state_dir: &'a Path,
    pub session: &'a Session,
}

impl Workflow<'_> {
    pub async fn run(&self, idle: impl Fn() -> bool) -> Result<Outcome, WorkflowError> {
        self.run_inner(idle, false).await
    }

    pub async fn run_automatic(&self, idle: impl Fn() -> bool) -> Result<Outcome, WorkflowError> {
        self.run_inner(idle, true).await
    }

    async fn run_inner(
        &self,
        idle: impl Fn() -> bool,
        automatic: bool,
    ) -> Result<Outcome, WorkflowError> {
        #[cfg(not(target_os = "linux"))]
        let _ = automatic;
        let _lock = self.lock()?;
        if !idle() {
            return Ok(Outcome::Deferred);
        }

        let mut state = self.load()?;
        let objects = ObjectStore::open(self.state_dir.join("objects"))?;
        if state.apply.is_some() {
            return self.finish_apply(&mut state, &objects, idle);
        }
        if state.publish.is_some() {
            return self.finish_publish(&mut state, &objects, idle).await;
        }
        if let Some(variant) = state.conflict {
            return Ok(Outcome::Conflict { variant });
        }

        let head = self.session.head(self.game).await?;
        if head.is_none() && state.base.is_some() {
            return Err(WorkflowError::MissingHead);
        }
        let remote = match head {
            Some(id) if state.base == Some(id) && state.base_manifest.is_some() => {
                state.base_manifest.clone()
            }
            Some(id) => Some(self.session.revision(self.game, id).await?.manifest),
            None => None,
        };
        let base = if state.remapped {
            None
        } else {
            match state.base {
                Some(id) if head == Some(id) => remote.clone(),
                Some(id) => match &state.base_manifest {
                    Some(manifest) => Some(manifest.clone()),
                    None => match self.session.revision(self.game, id).await {
                        Ok(revision) => Some(revision.manifest),
                        Err(error) if error.is_expired_revision() => None,
                        Err(error) => return Err(error.into()),
                    },
                },
                None => None,
            }
        };
        #[cfg(target_os = "linux")]
        if automatic
            && !state.remapped
            && head == state.base
            && let Some(manifest) = remote.as_ref()
        {
            let path = self.state_dir.join("scan-cache.json");
            if &crate::scan_cache::scan(self.roots, &path)? == manifest {
                return Ok(Outcome::Unchanged);
            }
        }
        let local = scan(self.roots)?;
        let remote_manifest = remote.as_ref();
        let decision = if !state.remapped
            && state.base.is_some()
            && base.is_none()
            && remote_manifest != Some(&local)
        {
            Decision::Conflict
        } else {
            decide(base.as_ref(), &local, remote_manifest)
        };
        match decision {
            Decision::Unchanged if state.base == head && !state.remapped => Ok(Outcome::Unchanged),
            Decision::Unchanged | Decision::Adopt => {
                state.base = head;
                state.remapped = false;
                state.base_manifest = remote;
                self.save(&state)?;
                Ok(head.map_or(Outcome::Unchanged, Outcome::Synchronized))
            }
            Decision::Publish | Decision::Conflict => {
                let manifest = capture(self.roots, &objects)?;
                if manifest != local || !idle() {
                    return Ok(Outcome::Deferred);
                }
                state.publish = Some(PendingPublish {
                    request: Uuid::new_v4(),
                    base: if state.remapped { None } else { state.base },
                    manifest,
                    replace_local: None,
                    resolves: None,
                });
                self.save(&state)?;
                self.finish_publish(&mut state, &objects, idle).await
            }
            Decision::Download => {
                let remote = remote.ok_or(WorkflowError::MissingHead)?;
                self.download_objects(&objects, &remote).await?;
                if !idle() || scan(self.roots)? != local {
                    return Ok(Outcome::Deferred);
                }
                state.apply = Some(PendingApply {
                    revision: head.ok_or(WorkflowError::MissingHead)?,
                    local,
                    target: remote,
                });
                self.save(&state)?;
                self.finish_apply(&mut state, &objects, idle)
            }
        }
    }

    /// Restore a selected historic or conflict revision as a new head.
    /// The caller supplies the head that was visible when the selection was made.
    pub async fn choose_revision(
        &self,
        revision: i64,
        expected_head: i64,
        idle: impl Fn() -> bool,
    ) -> Result<Outcome, WorkflowError> {
        let _lock = self.lock()?;
        if !idle() {
            return Ok(Outcome::Deferred);
        }
        let mut state = self.load()?;
        if state.publish.is_some() || state.apply.is_some() {
            return Err(WorkflowError::Busy);
        }
        if self.session.head(self.game).await? != Some(expected_head) {
            return Err(WorkflowError::StaleSelection);
        }

        let objects = ObjectStore::open(self.state_dir.join("objects"))?;
        let selected = self.session.revision(self.game, revision).await?;
        let resolves = match state.conflict {
            Some(variant) if variant != selected.id => {
                match self.session.revision(self.game, variant).await {
                    Ok(conflict) if !conflict.published && conflict.resolved_by.is_none() => {
                        Some(variant)
                    }
                    Ok(_) => None,
                    Err(error) if error.is_expired_revision() => None,
                    Err(error) => return Err(error.into()),
                }
            }
            _ if !selected.published && selected.resolved_by.is_none() => Some(selected.id),
            _ => None,
        };
        let target = selected.manifest;
        if target
            .entries()
            .iter()
            .any(|entry| !self.roots.iter().any(|(root, _)| *root == entry.root))
        {
            return Err(ApplyError::Roots.into());
        }
        self.download_objects(&objects, &target).await?;
        let local = capture(self.roots, &objects)?;
        if !idle() {
            return Ok(Outcome::Deferred);
        }
        let request = Uuid::new_v4();
        let backups = self.state_dir.join("before-restore");
        fs::create_dir_all(&backups)?;
        let mut backup = tempfile::NamedTempFile::new_in(&backups)?;
        serde_json::to_writer(backup.as_file_mut(), &local)?;
        backup.as_file().sync_all()?;
        backup
            .persist_noclobber(backups.join(format!("{request}.json")))
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(&backups)?.sync_all()?;

        state.publish = Some(PendingPublish {
            request,
            base: Some(expected_head),
            manifest: target,
            replace_local: Some(local),
            resolves,
        });
        self.save(&state)?;
        self.finish_publish(&mut state, &objects, idle).await
    }

    fn lock(&self) -> Result<File, WorkflowError> {
        fs::create_dir_all(self.state_dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.state_dir.join("sync.lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => Err(WorkflowError::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    async fn download_objects(
        &self,
        objects: &ObjectStore,
        manifest: &Manifest,
    ) -> Result<(), WorkflowError> {
        let staging = self.state_dir.join("downloads");
        fs::create_dir_all(&staging)?;
        for entry in manifest.entries() {
            if objects.open_verified(&entry.digest, entry.size).is_ok() {
                continue;
            }
            let path = staging.join(&entry.digest);
            self.session.download(entry, &path).await?;
            objects.put(&entry.digest, entry.size, File::open(&path)?)?;
            fs::remove_file(path)?;
        }
        Ok(())
    }

    async fn finish_publish(
        &self,
        state: &mut State,
        objects: &ObjectStore,
        idle: impl Fn() -> bool,
    ) -> Result<Outcome, WorkflowError> {
        let pending = state.publish.as_ref().expect("pending publication");
        for entry in pending.manifest.entries() {
            let source = objects.open_verified(&entry.digest, entry.size)?;
            self.session
                .upload_file(entry, tokio::fs::File::from_std(source))
                .await?;
        }
        let publication = match self
            .session
            .publish_resolving(
                self.game,
                pending.request,
                pending.base,
                &pending.manifest,
                pending.resolves,
            )
            .await
        {
            Ok(publication) => publication,
            Err(error) if error.is_stale_resolution() => {
                state.publish = None;
                self.save(state)?;
                return Err(WorkflowError::StaleSelection);
            }
            Err(error) => return Err(error.into()),
        };
        let pending = state.publish.take().expect("pending publication");
        let outcome = if publication.conflict {
            let variant = *state.conflict.get_or_insert(publication.revision);
            Outcome::Conflict { variant }
        } else {
            state.conflict = None;
            if let Some(local) = pending.replace_local {
                state.apply = Some(PendingApply {
                    revision: publication.revision,
                    local,
                    target: pending.manifest,
                });
                self.save(state)?;
                return self.finish_apply(state, objects, idle);
            }
            state.base = Some(publication.revision);
            state.remapped = false;
            state.base_manifest = Some(pending.manifest);
            Outcome::Synchronized(publication.revision)
        };
        self.save(state)?;
        Ok(outcome)
    }

    fn finish_apply(
        &self,
        state: &mut State,
        objects: &ObjectStore,
        idle: impl Fn() -> bool,
    ) -> Result<Outcome, WorkflowError> {
        let pending = state.apply.as_ref().expect("pending application");
        let journal = self.state_dir.join("restore");
        let restore = Restore::new(&journal, objects);
        let result = restore.recover(self.roots, &idle).and_then(|resumed| {
            if !resumed && scan(self.roots)? != pending.target {
                restore.apply(self.roots, &pending.local, &pending.target, &idle)?;
            }
            Ok(())
        });
        if matches!(result, Err(ApplyError::GameActive)) {
            return Ok(Outcome::Deferred);
        }
        result?;
        let revision = pending.revision;
        state.base = Some(revision);
        state.remapped = false;
        state.base_manifest = Some(pending.target.clone());
        state.apply = None;
        self.save(state)?;
        Ok(Outcome::Synchronized(revision))
    }

    fn load(&self) -> Result<State, WorkflowError> {
        let file = match File::open(self.state_dir.join("state.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
            Err(error) => return Err(error.into()),
        };
        let record: Record = serde_json::from_reader(file)?;
        if record.game != self.game || record.device != self.session.device() {
            return Err(WorkflowError::Identity);
        }
        let mut state = record.state;
        if let Some(mut previous) = record.roots {
            previous.sort_unstable();
            let mut current = self.roots.to_vec();
            current.sort_unstable();
            if previous != current {
                if state.publish.is_some() || state.apply.is_some() || state.conflict.is_some() {
                    return Err(WorkflowError::PendingRemap);
                }
                state.remapped = true;
                self.save(&state)?;
            }
        } else if state.publish.is_none() && state.apply.is_none() && state.conflict.is_none() {
            state.remapped = state.base.is_some();
            self.save(&state)?;
        }
        Ok(state)
    }

    fn save(&self, state: &State) -> Result<(), WorkflowError> {
        #[derive(Serialize)]
        struct BorrowedRecord<'a> {
            game: Uuid,
            device: Uuid,
            state: &'a State,
            roots: &'a [(Uuid, PathBuf)],
        }
        let mut file = tempfile::NamedTempFile::new_in(self.state_dir)?;
        serde_json::to_writer(
            file.as_file_mut(),
            &BorrowedRecord {
                game: self.game,
                device: self.session.device(),
                state,
                roots: self.roots,
            },
        )?;
        file.as_file().sync_all()?;
        file.persist(self.state_dir.join("state.json"))
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(self.state_dir)?.sync_all()?;
        Ok(())
    }
}

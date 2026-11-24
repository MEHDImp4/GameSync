use crate::{ScanError, capture, scan};
#[cfg(not(target_os = "linux"))]
use crate::{is_link, open_save};
use gamesync_core::{FileEntry, Manifest};
use gamesync_storage::{ObjectError, ObjectStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Journal(#[from] serde_json::Error),
    #[error("restore is already pending")]
    Pending,
    #[error("save roots changed or are unsafe")]
    Roots,
    #[error("local save changed: {0}")]
    Changed(String),
    #[error("the game must be closed before files can be changed")]
    GameActive,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    roots: Vec<(Uuid, PathBuf)>,
    before: Manifest,
    target: Manifest,
    #[serde(default)]
    staging: Option<Uuid>,
}

fn stage_name(id: Uuid, index: usize) -> String {
    format!(".gamesync-{id}-{index}.tmp")
}

impl Journal {
    fn root(&self, id: Uuid) -> Result<&Path, ApplyError> {
        let index = self
            .roots
            .binary_search_by_key(&id, |(id, _)| *id)
            .map_err(|_| ApplyError::Roots)?;
        Ok(&self.roots[index].1)
    }
}

type EntryMap<'a> = BTreeMap<(Uuid, &'a str), &'a FileEntry>;

fn indexed(manifest: &Manifest) -> EntryMap<'_> {
    manifest
        .entries()
        .iter()
        .map(|entry| ((entry.root, entry.path.as_str()), entry))
        .collect()
}

/// A recovery journal and lock in a private directory for one game.
pub struct Restore<'a> {
    state: &'a Path,
    objects: &'a ObjectStore,
}

impl<'a> Restore<'a> {
    pub fn new(state: &'a Path, objects: &'a ObjectStore) -> Self {
        Self { state, objects }
    }

    pub fn apply(
        &self,
        roots: &[(Uuid, PathBuf)],
        expected: &Manifest,
        target: &Manifest,
        idle: impl FnMut() -> bool,
    ) -> Result<(), ApplyError> {
        let _guard = self.lock()?;
        let path = self.state.join("apply.json");
        if path.try_exists()? {
            return Err(ApplyError::Pending);
        }
        let roots = checked_roots(roots)?;
        self.validate_target(&roots, target)?;
        let before = capture(&roots, self.objects)?;
        if &before != expected {
            return Err(ApplyError::Changed("manifest".into()));
        }

        let staging = loop {
            let id = Uuid::new_v4();
            let marker = format!(".gamesync-{id}-");
            if before
                .entries()
                .iter()
                .chain(target.entries())
                .all(|entry| {
                    !entry
                        .path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.starts_with(&marker))
                })
            {
                break id;
            }
        };
        let journal = Journal {
            roots,
            before,
            target: target.clone(),
            staging: Some(staging),
        };
        let mut temporary = tempfile::NamedTempFile::new_in(self.state)?;
        serde_json::to_writer(temporary.as_file_mut(), &journal)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| error.error)?;
        sync_directory(self.state)?;
        self.finish(&journal, idle)
    }

    /// Resume only when every local file still matches its original or intended contents.
    pub fn recover(
        &self,
        roots: &[(Uuid, PathBuf)],
        idle: impl FnMut() -> bool,
    ) -> Result<bool, ApplyError> {
        let _guard = self.lock()?;
        let journal: Journal = match File::open(self.state.join("apply.json")) {
            Ok(file) => serde_json::from_reader(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if checked_roots(roots)? != journal.roots {
            return Err(ApplyError::Roots);
        }
        self.validate_target(&journal.roots, &journal.target)?;
        let mut idle = idle;
        if let Some(staging) = journal.staging {
            for (index, entry) in journal.target.entries().iter().enumerate() {
                let root = journal.root(entry.root)?;
                clean_staged(
                    self.objects,
                    root,
                    entry,
                    &stage_name(staging, index),
                    &mut idle,
                )?;
            }
        }
        self.finish(&journal, idle)?;
        Ok(true)
    }

    fn lock(&self) -> Result<File, ApplyError> {
        fs::create_dir_all(self.state)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.state.join("apply.lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    fn validate_target(
        &self,
        roots: &[(Uuid, PathBuf)],
        target: &Manifest,
    ) -> Result<(), ApplyError> {
        for entry in target.entries() {
            if roots
                .binary_search_by_key(&entry.root, |(id, _)| *id)
                .is_err()
            {
                return Err(ApplyError::Roots);
            }
            self.objects.open_verified(&entry.digest, entry.size)?;
        }
        Ok(())
    }

    fn finish(&self, journal: &Journal, mut idle: impl FnMut() -> bool) -> Result<(), ApplyError> {
        let before = indexed(&journal.before);
        let target = indexed(&journal.target);
        let stage_id = journal.staging.unwrap_or_else(Uuid::new_v4);
        let names: BTreeMap<_, _> = journal
            .target
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                (
                    (entry.root, entry.path.as_str()),
                    stage_name(stage_id, index),
                )
            })
            .collect();
        let current = scan(&journal.roots)?;
        let current = indexed(&current);
        let paths: BTreeSet<_> = before
            .keys()
            .chain(target.keys())
            .chain(current.keys())
            .copied()
            .collect();
        for key in &paths {
            if current.get(key) != before.get(key) && current.get(key) != target.get(key) {
                return Err(ApplyError::Changed(key.1.into()));
            }
        }
        for key @ (root_id, relative) in paths {
            // Compare again at the write boundary, including paths that were absent.
            let old = current.get(&key).copied();
            let new = target.get(&key).copied();
            if old == new {
                continue;
            }
            if !idle() {
                return Err(ApplyError::GameActive);
            }
            let root = journal.root(root_id)?;
            apply_file(
                self.objects,
                root,
                relative,
                old,
                new,
                names.get(&key).map(String::as_str),
                &mut idle,
            )?;
        }
        if scan(&journal.roots)? != journal.target {
            return Err(ApplyError::Changed("manifest".into()));
        }
        fs::remove_file(self.state.join("apply.json"))?;
        sync_directory(self.state)?;
        Ok(())
    }
}

fn checked_roots(roots: &[(Uuid, PathBuf)]) -> Result<Vec<(Uuid, PathBuf)>, ApplyError> {
    let mut checked: Vec<(Uuid, PathBuf)> = Vec::new();
    for (id, root) in roots {
        #[cfg(target_os = "linux")]
        let root = {
            crate::linux::open_root(root)?;
            root.components().collect::<PathBuf>()
        };
        #[cfg(not(target_os = "linux"))]
        let root = {
            #[cfg(windows)]
            let _guard = crate::windows::lock_chain(root)?;
            for ancestor in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
                let metadata = fs::symlink_metadata(ancestor)?;
                if is_link(&metadata) || !metadata.is_dir() {
                    return Err(ApplyError::Roots);
                }
            }
            root.canonicalize()?
        };
        if checked.iter().any(|(other_id, other)| {
            id == other_id || root.starts_with(other) || other.starts_with(&root)
        }) {
            return Err(ApplyError::Roots);
        }
        checked.push((*id, root));
    }
    checked.sort_unstable();
    Ok(checked)
}

#[cfg(target_os = "linux")]
use crate::linux::{apply_file, clean_staged};

#[cfg(not(target_os = "linux"))]
fn apply_file(
    objects: &ObjectStore,
    root: &Path,
    relative: &str,
    old: Option<&FileEntry>,
    new: Option<&FileEntry>,
    stage_name: Option<&str>,
    idle: &mut impl FnMut() -> bool,
) -> Result<(), ApplyError> {
    let (path, _guards) = destination(root, relative, new.is_some())?;
    let parent = path.parent().ok_or(ApplyError::Roots)?;
    if let Some(entry) = new {
        let mut source = objects.open_verified(&entry.digest, entry.size)?;
        let staged_path = parent.join(stage_name.ok_or(ApplyError::Roots)?);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged_path)?;
        let mut staged = tempfile::NamedTempFile::from_parts(
            file,
            tempfile::TempPath::try_from_path(staged_path)?,
        );
        io::copy(&mut source, staged.as_file_mut())?;
        staged.as_file().sync_all()?;
        if !idle() {
            return Err(ApplyError::GameActive);
        }
        if destination(root, relative, false)?.0 != path || !matches_entry(&path, old)? {
            return Err(ApplyError::Changed(relative.into()));
        }
        if old.is_some() {
            staged.persist(&path).map_err(|error| error.error)?;
        } else {
            staged
                .persist_noclobber(&path)
                .map_err(|error| error.error)?;
        }
    } else {
        if !matches_entry(&path, old)? {
            return Err(ApplyError::Changed(relative.into()));
        }
        fs::remove_file(&path)?;
    }
    sync_directory(parent)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn clean_staged(
    objects: &ObjectStore,
    root: &Path,
    entry: &FileEntry,
    name: &str,
    idle: &mut impl FnMut() -> bool,
) -> Result<(), ApplyError> {
    let (destination, _guards) = match destination(root, &entry.path, false) {
        Ok(value) => value,
        Err(ApplyError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let path = destination.with_file_name(name);
    let staged = match open_save(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let metadata = staged.metadata()?;
    if !metadata.is_file() || is_link(&metadata) || !matches_prefix(staged, objects, entry)? {
        return Err(ApplyError::Changed(path.to_string_lossy().into()));
    }
    if !idle() {
        return Err(ApplyError::GameActive);
    }
    fs::remove_file(&path)?;
    sync_directory(path.parent().ok_or(ApplyError::Roots)?)?;
    Ok(())
}

pub(crate) fn matches_prefix(
    mut staged: File,
    objects: &ObjectStore,
    entry: &FileEntry,
) -> Result<bool, ApplyError> {
    let mut remaining = staged.metadata()?.len();
    if remaining > entry.size {
        return Ok(false);
    }
    let mut source = objects.open_verified(&entry.digest, entry.size)?;
    let mut partial = [0; 32 * 1024];
    let mut expected = [0; 32 * 1024];
    while remaining != 0 {
        let size = remaining.min(partial.len() as u64) as usize;
        staged.read_exact(&mut partial[..size])?;
        source.read_exact(&mut expected[..size])?;
        if partial[..size] != expected[..size] {
            return Ok(false);
        }
        remaining -= size as u64;
    }
    Ok(staged.read(&mut partial[..1])? == 0)
}

#[cfg(not(target_os = "linux"))]
fn destination(
    root: &Path,
    relative: &str,
    create: bool,
) -> Result<(PathBuf, Vec<File>), ApplyError> {
    #[cfg(windows)]
    let mut guards = crate::windows::lock_chain(root)?;
    #[cfg(not(windows))]
    let guards = Vec::new();
    let mut components = relative.split('/').peekable();
    let mut path = root.to_owned();
    let metadata = fs::symlink_metadata(root)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(ApplyError::Roots);
    }
    while let Some(component) = components.next() {
        path.push(component);
        if components.peek().is_none() {
            break;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
            Ok(_) => return Err(ApplyError::Roots),
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                fs::create_dir(&path)?;
                sync_directory(path.parent().ok_or(ApplyError::Roots)?)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(windows)]
        guards.push(crate::windows::lock_directory(&path)?);
    }
    Ok((path, guards))
}

#[cfg(not(target_os = "linux"))]
fn matches_entry(path: &Path, expected: Option<&FileEntry>) -> Result<bool, ApplyError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(expected.is_none()),
        Err(error) => return Err(error.into()),
    };
    let Some(entry) = expected else {
        return Ok(false);
    };
    if is_link(&metadata) || !metadata.is_file() || metadata.len() != entry.size {
        return Ok(false);
    }
    let mut file = open_save(path)?;
    let opened = file.metadata()?;
    if is_link(&opened) || !opened.is_file() {
        return Ok(false);
    }
    let mut hasher = blake3::Hasher::new();
    let size = io::copy(&mut file, &mut hasher)?;
    Ok(size == entry.size && hasher.finalize().to_hex().as_str() == entry.digest)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

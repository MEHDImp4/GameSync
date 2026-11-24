use gamesync_core::{FileEntry, Manifest, ManifestError};
use gamesync_storage::{ObjectError, ObjectStore};
use std::{
    fs::{self, File},
    io::{self, Seek},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("unsupported file or directory: {0}")]
    Unsupported(PathBuf),
    #[error("file changed during scan: {0}")]
    Changed(PathBuf),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Object(#[from] ObjectError),
}

fn at(path: &Path, source: io::Error) -> ScanError {
    ScanError::Io {
        path: path.to_owned(),
        source,
    }
}

pub fn scan(roots: &[(Uuid, PathBuf)]) -> Result<Manifest, ScanError> {
    scan_inner(roots, None)
}

/// Capture a manifest and its verified contents into a separate private store.
pub fn capture(roots: &[(Uuid, PathBuf)], store: &ObjectStore) -> Result<Manifest, ScanError> {
    scan_inner(roots, Some(store))
}

#[cfg(not(target_os = "linux"))]
fn scan_inner(
    roots: &[(Uuid, PathBuf)],
    store: Option<&ObjectStore>,
) -> Result<Manifest, ScanError> {
    let mut entries = Vec::new();
    for (id, root) in roots {
        #[cfg(windows)]
        let _root_guard = windows::lock_chain(root).map_err(|error| at(root, error))?;
        let metadata = fs::symlink_metadata(root).map_err(|error| at(root, error))?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(ScanError::Unsupported(root.clone()));
        }
        let mut directories = vec![root.clone()];
        while let Some(directory) = directories.pop() {
            #[cfg(windows)]
            let _directory_guard =
                windows::lock_chain(&directory).map_err(|error| at(&directory, error))?;
            for entry in fs::read_dir(&directory).map_err(|error| at(&directory, error))? {
                let path = entry.map_err(|error| at(&directory, error))?.path();
                let metadata = fs::symlink_metadata(&path).map_err(|error| at(&path, error))?;
                if is_link(&metadata) {
                    return Err(ScanError::Unsupported(path));
                }
                if metadata.is_dir() {
                    directories.push(path);
                    continue;
                }
                if !metadata.is_file() {
                    return Err(ScanError::Unsupported(path));
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("directory walk stays beneath root")
                    .iter()
                    .map(|name| {
                        name.to_str()
                            .ok_or_else(|| ScanError::Unsupported(path.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                gamesync_core::validate_path(&relative)?;
                let file = open_save(&path).map_err(|error| at(&path, error))?;
                entries.push(read_entry(file, *id, relative, &path, store)?);
            }
        }
    }
    Ok(Manifest::new(entries)?)
}

fn read_entry(
    mut file: File,
    root: Uuid,
    relative: String,
    path: &Path,
    store: Option<&ObjectStore>,
) -> Result<FileEntry, ScanError> {
    let before = file.metadata().map_err(|error| at(path, error))?;
    if !before.is_file() || is_link(&before) {
        return Err(ScanError::Unsupported(path.to_owned()));
    }
    let mut hasher = blake3::Hasher::new();
    let size = io::copy(&mut file, &mut hasher).map_err(|error| at(path, error))?;
    let after = file.metadata().map_err(|error| at(path, error))?;
    if size != before.len()
        || size != after.len()
        || before.modified().map_err(|error| at(path, error))?
            != after.modified().map_err(|error| at(path, error))?
    {
        return Err(ScanError::Changed(path.to_owned()));
    }
    let digest = hasher.finalize().to_hex().to_string();
    if let Some(store) = store {
        file.rewind().map_err(|error| at(path, error))?;
        store.put(&digest, size, file)?;
    }
    Ok(FileEntry {
        root,
        path: relative,
        size,
        digest,
    })
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod scan_cache;
#[cfg(target_os = "linux")]
use linux::scan_inner;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn open_save(path: &Path) -> io::Result<File> {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};

    const FILE_SHARE_READ: u32 = 1;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_save(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

mod apply;
pub use apply::{ApplyError, Restore};

mod workflow;
pub use workflow::{Outcome, Workflow, WorkflowError};

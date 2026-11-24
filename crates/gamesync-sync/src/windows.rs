use crate::is_link;
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::Path,
};

/// Keep a directory entry in place while a descendant is accessed by pathname.
pub fn lock_directory(path: &Path) -> io::Result<File> {
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsafe save directory",
        ));
    }
    Ok(file)
}

pub fn lock_chain(path: &Path) -> io::Result<Vec<File>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute save directory",
        ));
    }
    let parents: Vec<_> = path
        .ancestors()
        .filter(|parent| parent.has_root())
        .collect();
    let mut handles = Vec::with_capacity(parents.len());
    for parent in parents.into_iter().rev() {
        handles.push(lock_directory(parent)?);
    }
    Ok(handles)
}

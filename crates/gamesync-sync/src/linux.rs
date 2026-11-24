use crate::{
    ApplyError, ScanError, at, read_entry,
    scan_cache::{Fingerprint, ScanPass},
};
use gamesync_core::{FileEntry, Manifest};
use gamesync_storage::ObjectStore;
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
use std::{
    fs::File,
    io,
    os::fd::{AsFd, OwnedFd},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

pub(crate) fn open_root(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute save root",
        ));
    }
    let mut current = fs::open("/", DIRECTORY, Mode::empty())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = fs::openat(&current, name, DIRECTORY, Mode::empty())?
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid save root component",
                ));
            }
        }
    }
    Ok(current)
}

pub fn scan_inner(
    roots: &[(Uuid, PathBuf)],
    store: Option<&ObjectStore>,
) -> Result<Manifest, ScanError> {
    walk(roots, store, None)
}

pub(crate) fn scan_cached(
    roots: &[(Uuid, PathBuf)],
    pass: &mut ScanPass,
) -> Result<Manifest, ScanError> {
    walk(roots, None, Some(pass))
}

fn walk(
    roots: &[(Uuid, PathBuf)],
    store: Option<&ObjectStore>,
    mut cache: Option<&mut ScanPass>,
) -> Result<Manifest, ScanError> {
    let mut entries = Vec::new();
    for (root, path) in roots {
        let fd = open_root(path).map_err(|error| at(path, error))?;
        scan_directory(*root, path, "", &fd, store, &mut entries, &mut cache)?;
        let current = open_root(path).map_err(|error| at(path, error))?;
        if !same_directory(&fd, &current).map_err(|error| at(path, error))? {
            return Err(ScanError::Changed(path.clone()));
        }
    }
    Ok(Manifest::new(entries)?)
}

fn scan_directory(
    root: Uuid,
    base: &Path,
    prefix: &str,
    directory: &OwnedFd,
    store: Option<&ObjectStore>,
    entries: &mut Vec<FileEntry>,
    cache: &mut Option<&mut ScanPass>,
) -> Result<(), ScanError> {
    let directory_path = base.join(prefix);
    if prefix.matches('/').count() > 128 {
        return Err(ScanError::Unsupported(directory_path));
    }
    let dir = Dir::read_from(directory).map_err(|error| at(&directory_path, error.into()))?;
    for entry in dir {
        let entry = entry.map_err(|error| at(&directory_path, error.into()))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let name = name
            .to_str()
            .map_err(|_| ScanError::Unsupported(directory_path.clone()))?;
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let path = base.join(&relative);
        gamesync_core::validate_path(&relative)?;
        let stat = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| at(&path, error.into()))?;
        let kind = FileType::from_raw_mode(stat.st_mode);
        let opened = match kind {
            FileType::Directory => {
                let child = fs::openat(directory, name, DIRECTORY, Mode::empty())
                    .map_err(|error| at(&path, error.into()))?;
                scan_directory(root, base, &relative, &child, store, entries, cache)?;
                fs::fstat(&child).map_err(|error| at(&path, error.into()))?
            }
            FileType::RegularFile => {
                let file = fs::openat(directory, name, READ, Mode::empty())
                    .map_err(|error| at(&path, error.into()))?;
                let opened = fs::fstat(&file).map_err(|error| at(&path, error.into()))?;
                let fingerprint = Fingerprint::from_stat(&opened);
                if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
                    || fingerprint != Fingerprint::from_stat(&stat)
                {
                    return Err(ScanError::Changed(path));
                }
                let entry = cache
                    .as_deref_mut()
                    .and_then(|pass| pass.digest(root, &relative, fingerprint));
                if let Some(entry) = entry {
                    entries.push(entry);
                } else {
                    let reader = rustix::io::dup(&file).map_err(|error| at(&path, error.into()))?;
                    let entry = read_entry(File::from(reader), root, relative, &path, store)?;
                    if let Some(pass) = cache.as_deref_mut() {
                        pass.record(&entry, fingerprint);
                    }
                    entries.push(entry);
                }
                opened
            }
            _ => return Err(ScanError::Unsupported(path)),
        };
        let current = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| at(&path, error.into()))?;
        if (opened.st_dev, opened.st_ino) != (stat.st_dev, stat.st_ino)
            || (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino)
            || kind == FileType::RegularFile
                && Fingerprint::from_stat(&stat) != Fingerprint::from_stat(&current)
        {
            return Err(ScanError::Changed(path));
        }
    }
    Ok(())
}

fn parent(root: &Path, relative: &str, create: bool) -> io::Result<OwnedFd> {
    let mut fd = open_root(root)?;
    let Some((parents, _)) = relative.rsplit_once('/') else {
        return Ok(fd);
    };
    for name in parents.split('/') {
        fd = match fs::openat(&fd, name, DIRECTORY, Mode::empty()) {
            Err(rustix::io::Errno::NOENT) if create => {
                fs::mkdirat(&fd, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
                fs::fsync(&fd)?;
                fs::openat(&fd, name, DIRECTORY, Mode::empty())?
            }
            result => result?,
        };
    }
    Ok(fd)
}

fn same_directory(first: impl AsFd, second: impl AsFd) -> io::Result<bool> {
    let first = fs::fstat(first)?;
    let second = fs::fstat(second)?;
    Ok((first.st_dev, first.st_ino) == (second.st_dev, second.st_ino))
}

fn matches_entry(directory: &OwnedFd, name: &str, old: Option<&FileEntry>) -> io::Result<bool> {
    let stat = match fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(old.is_none()),
        Err(error) => return Err(error.into()),
    };
    let Some(old) = old else { return Ok(false) };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Ok(false);
    }
    let fd = fs::openat(directory, name, READ, Mode::empty())?;
    let opened = fs::fstat(&fd)?;
    if (stat.st_dev, stat.st_ino) != (opened.st_dev, opened.st_ino)
        || opened.st_size as u64 != old.size
        || FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
    {
        return Ok(false);
    }
    let mut hasher = blake3::Hasher::new();
    let size = io::copy(&mut File::from(fd), &mut hasher)?;
    let current = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(size == old.size
        && hasher.finalize().to_hex().as_str() == old.digest
        && (
            opened.st_dev,
            opened.st_ino,
            opened.st_size,
            opened.st_mtime,
            opened.st_mtime_nsec,
        ) == (
            current.st_dev,
            current.st_ino,
            current.st_size,
            current.st_mtime,
            current.st_mtime_nsec,
        ))
}

struct Staged<'a> {
    directory: &'a OwnedFd,
    name: String,
    file: File,
}

impl Staged<'_> {
    fn is_original(&self) -> io::Result<bool> {
        let named = fs::statat(
            self.directory,
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        let written = fs::fstat(&self.file)?;
        Ok((named.st_dev, named.st_ino) == (written.st_dev, written.st_ino))
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if self.is_original().unwrap_or(false) {
            let _ = fs::unlinkat(self.directory, self.name.as_str(), AtFlags::empty());
        }
    }
}

pub fn apply_file(
    objects: &ObjectStore,
    root: &Path,
    relative: &str,
    old: Option<&FileEntry>,
    new: Option<&FileEntry>,
    stage_name: Option<&str>,
    idle: &mut impl FnMut() -> bool,
) -> Result<(), ApplyError> {
    let directory = parent(root, relative, new.is_some())?;
    let name = relative
        .rsplit('/')
        .next()
        .expect("validated relative file");
    let operation = || -> Result<(), ApplyError> {
        let refreshed = parent(root, relative, false)?;
        if !same_directory(&directory, &refreshed)? || !matches_entry(&directory, name, old)? {
            return Err(ApplyError::Changed(relative.into()));
        }
        Ok(())
    };
    if let Some(entry) = new {
        let mut source = objects.open_verified(&entry.digest, entry.size)?;
        let staged_name = stage_name.ok_or(ApplyError::Roots)?;
        let fd = fs::openat(
            &directory,
            staged_name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        let mut stage = Staged {
            directory: &directory,
            name: staged_name.into(),
            file: File::from(fd),
        };
        io::copy(&mut source, &mut stage.file)?;
        stage.file.sync_all()?;
        if !idle() {
            return Err(ApplyError::GameActive);
        }
        operation()?;
        if !stage.is_original()? {
            return Err(ApplyError::Changed(relative.into()));
        }
        if old.is_some() {
            fs::renameat(&directory, stage.name.as_str(), &directory, name)
                .map_err(io::Error::from)?;
        } else {
            fs::renameat_with(
                &directory,
                stage.name.as_str(),
                &directory,
                name,
                fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)?;
        }
        drop(stage);
    } else {
        operation()?;
        fs::unlinkat(&directory, name, AtFlags::empty()).map_err(io::Error::from)?;
    }
    fs::fsync(&directory).map_err(io::Error::from)?;
    Ok(())
}

pub fn clean_staged(
    objects: &ObjectStore,
    root: &Path,
    entry: &FileEntry,
    name: &str,
    idle: &mut impl FnMut() -> bool,
) -> Result<(), ApplyError> {
    let directory = match parent(root, &entry.path, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let before = match fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(io::Error::from(error).into()),
    };
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(ApplyError::Changed(name.into()));
    }
    let staged = fs::openat(&directory, name, READ, Mode::empty()).map_err(io::Error::from)?;
    let opened = fs::fstat(&staged).map_err(io::Error::from)?;
    if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)
        || !crate::apply::matches_prefix(File::from(staged), objects, entry)?
    {
        return Err(ApplyError::Changed(name.into()));
    }
    if !idle() {
        return Err(ApplyError::GameActive);
    }
    let refreshed = parent(root, &entry.path, false)?;
    let current =
        fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    if !same_directory(&directory, &refreshed)?
        || (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_ctime,
            before.st_ctime_nsec,
        ) != (
            current.st_dev,
            current.st_ino,
            current.st_size,
            current.st_ctime,
            current.st_ctime_nsec,
        )
    {
        return Err(ApplyError::Changed(name.into()));
    }
    fs::unlinkat(&directory, name, AtFlags::empty()).map_err(io::Error::from)?;
    fs::fsync(&directory).map_err(io::Error::from)?;
    Ok(())
}

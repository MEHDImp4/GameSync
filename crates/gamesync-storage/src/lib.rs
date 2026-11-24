use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    #[error("invalid object digest")]
    InvalidDigest,
    #[error("object size or digest does not match the manifest")]
    Integrity,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Content-addressed storage in a private, application-owned directory.
pub struct ObjectStore {
    directory: PathBuf,
}

impl ObjectStore {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, ObjectError> {
        fs::create_dir_all(&directory)?;
        Ok(Self {
            directory: directory.as_ref().canonicalize()?,
        })
    }

    pub fn put(&self, digest: &str, size: u64, mut reader: impl Read) -> Result<(), ObjectError> {
        let destination = self.object_path(digest)?;
        let mut staged = tempfile::NamedTempFile::new_in(&self.directory)?;
        transfer_verified(&mut reader, staged.as_file_mut(), digest, size)?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&destination) {
            Ok(_) => {
                #[cfg(unix)]
                File::open(&self.directory)?.sync_all()?;
                Ok(())
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                self.open_verified(digest, size)?;
                Ok(())
            }
            Err(error) => Err(error.error.into()),
        }
    }

    pub fn open_verified(&self, digest: &str, size: u64) -> Result<File, ObjectError> {
        let mut file = File::open(self.object_path(digest)?)?;
        transfer_verified(&mut file, io::sink(), digest, size)?;
        file.rewind()?;
        Ok(file)
    }

    pub fn usage(&self) -> Result<(u64, u64), ObjectError> {
        let mut count = 0_u64;
        let mut bytes = 0_u64;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_file()
                && name
                    .to_str()
                    .is_some_and(|digest| self.object_path(digest).is_ok())
            {
                count = count.saturating_add(1);
                bytes = bytes.saturating_add(entry.metadata()?.len());
            }
        }
        Ok((count, bytes))
    }

    fn remove_unreferenced(
        &self,
        referenced: &HashSet<String>,
        now: SystemTime,
        grace: Duration,
    ) -> Result<usize, ObjectError> {
        let mut removed = 0;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(digest) = name.to_str() else {
                continue;
            };
            if referenced.contains(digest)
                || self.object_path(digest).is_err()
                || !entry.file_type()?.is_file()
            {
                continue;
            }
            let metadata = entry.metadata()?;
            if !now
                .duration_since(metadata.modified()?)
                .is_ok_and(|age| age >= grace)
            {
                continue;
            }
            fs::remove_file(entry.path())?;
            removed += 1;
        }
        #[cfg(unix)]
        if removed != 0 {
            File::open(&self.directory)?.sync_all()?;
        }
        Ok(removed)
    }

    fn object_path(&self, digest: &str) -> Result<PathBuf, ObjectError> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ObjectError::InvalidDigest);
        }
        Ok(self.directory.join(digest))
    }
}

fn transfer_verified(
    mut reader: impl Read,
    mut writer: impl Write,
    digest: &str,
    expected_size: u64,
) -> Result<(), ObjectError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut remaining = expected_size;
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(ObjectError::Integrity);
        }
        remaining -= read as u64;
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
    }
    if reader.read(&mut buffer[..1])? != 0 || hasher.finalize().to_hex().as_str() != digest {
        return Err(ObjectError::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_deduplicates_only_verified_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::open(dir.path()).unwrap();
        let digest = blake3::hash(b"original").to_hex();
        let digest = digest.as_str();
        assert!(store.put(digest, 7, &b"original"[..]).is_err());
        assert!(store.put(digest, 9, &b"original"[..]).is_err());
        assert!(store.put(digest, 8, &b"modified"[..]).is_err());
        assert_eq!(dir.path().read_dir().unwrap().count(), 0);
        store.put(digest, 8, &b"original"[..]).unwrap();
        store.put(digest, 8, &b"original"[..]).unwrap();
        assert_eq!(dir.path().read_dir().unwrap().count(), 1);
        let reopened = ObjectStore::open(dir.path()).unwrap();
        let mut bytes = Vec::new();
        reopened
            .open_verified(digest, 8)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"original");
        fs::write(dir.path().join(digest), b"modified").unwrap();
        assert!(matches!(
            reopened.open_verified(digest, 8),
            Err(ObjectError::Integrity)
        ));
    }
}

mod revisions;
pub use revisions::{
    Cleanup, Publication, PublishInput, Revision, RevisionError, RevisionSummary, Revisions,
};

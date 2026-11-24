use gamesync_core::{FileEntry, Manifest};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const FULL_SCAN_INTERVAL: u64 = 30 * 60;
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Fingerprint {
    pub(crate) fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            size: stat.st_size as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec as i64),
            changed: (stat.st_ctime, stat.st_ctime_nsec as i64),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    entry: FileEntry,
    fingerprint: Fingerprint,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    last_full: u64,
    files: Vec<Entry>,
}

pub(crate) struct ScanPass {
    input: BTreeMap<(Uuid, String), Entry>,
    output: Vec<Entry>,
    last_full: u64,
    hits: usize,
    hashed: usize,
}

impl ScanPass {
    pub(crate) fn digest(
        &mut self,
        root: Uuid,
        relative: &str,
        fingerprint: Fingerprint,
    ) -> Option<FileEntry> {
        let cached = self.input.remove(&(root, relative.to_owned()))?;
        if cached.fingerprint != fingerprint {
            return None;
        }
        self.hits += 1;
        let entry = cached.entry.clone();
        self.output.push(cached);
        Some(entry)
    }

    pub(crate) fn record(&mut self, entry: &FileEntry, fingerprint: Fingerprint) {
        self.hashed += 1;
        self.output.push(Entry {
            entry: entry.clone(),
            fingerprint,
        });
    }
}

pub(crate) fn scan(
    roots: &[(Uuid, std::path::PathBuf)],
    path: &Path,
) -> Result<Manifest, crate::ScanError> {
    scan_at(roots, path, now()).map(|(manifest, _)| manifest)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn scan_at(
    roots: &[(Uuid, std::path::PathBuf)],
    path: &Path,
    time: u64,
) -> Result<(Manifest, usize), crate::ScanError> {
    let old = match load(path) {
        Ok(cache) => cache,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Cache::default(),
        Err(error) => {
            eprintln!("GameSync scan cache ignored: {error}");
            Cache::default()
        }
    };
    let last_full = old.last_full;
    let reuse = last_full != 0
        && time
            .checked_sub(last_full)
            .is_some_and(|elapsed| elapsed < FULL_SCAN_INTERVAL);
    let input = if reuse {
        old.files
            .into_iter()
            .map(|value| ((value.entry.root, value.entry.path.clone()), value))
            .collect()
    } else {
        BTreeMap::new()
    };
    let old_count = input.len();
    let mut pass = ScanPass {
        input,
        output: Vec::new(),
        last_full: if reuse { last_full } else { time },
        hits: 0,
        hashed: 0,
    };
    let manifest = crate::linux::scan_cached(roots, &mut pass)?;
    if (!reuse || pass.hashed != 0 || old_count != pass.output.len())
        && let Err(error) = save(path, pass.last_full, pass.output)
    {
        eprintln!("GameSync scan cache not written: {error}");
    }
    Ok((manifest, pass.hits))
}

fn load(path: &Path) -> io::Result<Cache> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_CACHE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "scan cache too large",
        ));
    }
    let cache: Cache = serde_json::from_reader(file)?;
    // Treat an invalid cache as a miss rather than returning an unverified digest.
    Manifest::new(cache.files.iter().map(|item| item.entry.clone()).collect())
        .map_err(io::Error::other)?;
    Ok(cache)
}

fn save(path: &Path, last_full: u64, files: Vec<Entry>) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache parent required"))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), &Cache { last_full, files })?;
    if temp.as_file().metadata()?.len() <= MAX_CACHE_BYTES {
        temp.persist(path).map_err(|error| error.error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_stable_fingerprint_and_rehashes_after_change_or_timeout() {
        let data = tempfile::tempdir().unwrap();
        let root = data.path().join("saves");
        fs::create_dir(&root).unwrap();
        let save = root.join("save");
        fs::write(&save, b"abc").unwrap();
        let timestamp = fs::metadata(&save).unwrap().modified().unwrap();
        let path = data.path().join("scan-cache.json");
        let roots = [(Uuid::new_v4(), root)];

        let (before, hits) = scan_at(&roots, &path, 1).unwrap();
        assert_eq!(hits, 0);
        assert_eq!(scan_at(&roots, &path, 2).unwrap(), (before.clone(), 1));
        let replacement = data.path().join("replacement");
        fs::write(&replacement, b"xyz").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        fs::rename(replacement, save).unwrap();
        let (after, hits) = scan_at(&roots, &path, 3).unwrap();
        assert_eq!(hits, 0);
        assert_ne!(before, after);
        assert_eq!(
            scan_at(&roots, &path, FULL_SCAN_INTERVAL + 1).unwrap(),
            (after, 0)
        );
    }
}

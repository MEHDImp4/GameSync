use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub root: Uuid,
    pub path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Manifest(Vec<FileEntry>);

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unsafe relative path: {0}")]
    Path(String),
    #[error("conflicting manifest path: {0}")]
    Collision(String),
    #[error("invalid BLAKE3 digest for {0}")]
    Digest(String),
}

impl Manifest {
    pub fn new(mut entries: Vec<FileEntry>) -> Result<Self, ManifestError> {
        entries.sort_unstable_by(|a, b| (a.root, &a.path).cmp(&(b.root, &b.path)));
        let mut files = BTreeSet::new();
        let mut spellings = BTreeMap::new();
        for entry in &entries {
            validate_path(&entry.path)?;
            if entry.digest.len() != 64
                || !entry
                    .digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(ManifestError::Digest(entry.path.clone()));
            }
            for (index, _) in entry
                .path
                .match_indices('/')
                .chain([(entry.path.len(), "")])
            {
                let prefix = &entry.path[..index];
                let key = (entry.root, prefix.to_uppercase());
                if files.contains(&key)
                    || spellings
                        .insert(key.clone(), prefix)
                        .is_some_and(|existing| existing != prefix)
                {
                    return Err(ManifestError::Collision(entry.path.clone()));
                }
                if index == entry.path.len() {
                    files.insert(key);
                }
            }
        }
        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.0
    }
}

pub fn validate_path(path: &str) -> Result<(), ManifestError> {
    for component in path.split('/') {
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if component.is_empty()
            || component.ends_with(['.', ' '])
            || reserved
            || component
                .chars()
                .any(|ch| ch.is_control() || "<>:\"\\|?*".contains(ch))
        {
            return Err(ManifestError::Path(path.to_owned()));
        }
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub enum Decision {
    Unchanged,
    Publish,
    Download,
    Adopt,
    Conflict,
}

pub fn decide(base: Option<&Manifest>, local: &Manifest, remote: Option<&Manifest>) -> Decision {
    match (base, remote) {
        (_, None) if local.entries().is_empty() && base.is_none() => Decision::Unchanged,
        (None, None) => Decision::Publish,
        (Some(_), None) => Decision::Conflict,
        (_, Some(head)) if head == local => {
            if base == Some(local) {
                Decision::Unchanged
            } else {
                Decision::Adopt
            }
        }
        (None, Some(_)) if local.entries().is_empty() => Decision::Download,
        (Some(previous), Some(_)) if previous == local => Decision::Download,
        (Some(previous), Some(head)) if previous == head => Decision::Publish,
        _ => Decision::Conflict,
    }
}

pub fn retain_revision(
    rank: usize,
    age_days: u64,
    recent_count: usize,
    minimum_days: u64,
    pinned: bool,
) -> bool {
    pinned || rank < recent_count || age_days < minimum_days
}

impl<'de> Deserialize<'de> for Manifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameRoot {
    pub id: Uuid,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameRegistration {
    pub id: Uuid,
    pub title: String,
    pub roots: Vec<GameRoot>,
}

impl GameRegistration {
    pub fn is_valid(&self) -> bool {
        fn label(value: &str) -> bool {
            !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        }
        !self.id.is_nil()
            && label(&self.title)
            && (1..=16).contains(&self.roots.len())
            && self
                .roots
                .iter()
                .all(|root| !root.id.is_nil() && label(&root.label))
            && self.root_ids().len() == self.roots.len()
    }

    pub fn root_ids(&self) -> BTreeSet<Uuid> {
        self.roots.iter().map(|root| root.id).collect()
    }
}

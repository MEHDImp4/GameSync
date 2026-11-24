use gamesync_protocol::{GameRegistration, GameRoot};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: Uuid,
    pub root: Uuid,
    pub title: String,
    pub directory: PathBuf,
    pub executable: PathBuf,
    #[serde(default = "enabled")]
    pub tracking: bool,
    #[serde(default)]
    pub additional_roots: Vec<SaveRoot>,
    #[serde(default = "first_root_label")]
    pub root_label: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SaveRoot {
    pub id: Uuid,
    pub label: String,
    pub directory: PathBuf,
}

fn enabled() -> bool {
    true
}

fn first_root_label() -> String {
    "Sauvegardes".into()
}

impl Game {
    pub fn paths(&self) -> impl Iterator<Item = (Uuid, &Path)> {
        std::iter::once((self.root, self.directory.as_path())).chain(
            self.additional_roots
                .iter()
                .map(|root| (root.id, root.directory.as_path())),
        )
    }

    pub fn registration(&self) -> GameRegistration {
        GameRegistration {
            id: self.id,
            title: self.title.clone(),
            roots: std::iter::once(GameRoot {
                id: self.root,
                label: self.root_label.clone(),
            })
            .chain(self.additional_roots.iter().map(|root| GameRoot {
                id: root.id,
                label: root.label.clone(),
            }))
            .collect(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Server {
    pub url: String,
    pub device: Uuid,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    #[default]
    Fr,
    En,
}

impl Locale {
    pub fn text<'a>(self, french: &'a str, english: &'a str) -> &'a str {
        match self {
            Self::Fr => french,
            Self::En => english,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub server: Option<Server>,
    pub games: Vec<Game>,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
    #[serde(default)]
    pub bandwidth_kib: u32,
    #[serde(default)]
    pub notifications: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub locale: Locale,
}

fn default_interval() -> u64 {
    300
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: None,
            games: Vec::new(),
            automatic: false,
            interval_seconds: default_interval(),
            bandwidth_kib: 0,
            notifications: false,
            theme: Theme::default(),
            locale: Locale::default(),
        }
    }
}

fn database(path: &Path) -> Result<Connection> {
    Connection::open(path.join("client.sqlite")).map_err(|error| error.to_string())
}

pub fn load(path: &Path) -> Result<Config> {
    let db = database(path)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS config (id INTEGER PRIMARY KEY CHECK (id = 1), value TEXT NOT NULL)")
        .map_err(|error| error.to_string())?;
    let value: Option<String> = db
        .query_row("SELECT value FROM config WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| error.to_string())?;
    value.map_or_else(
        || Ok(Config::default()),
        |json| serde_json::from_str(&json).map_err(|error| error.to_string()),
    )
}

pub fn save(path: &Path, config: &Config) -> Result<()> {
    let value = serde_json::to_string(config).map_err(|error| error.to_string())?;
    database(path)?
        .execute(
            "INSERT OR REPLACE INTO config (id, value) VALUES (1, ?)",
            [value],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_config_defaults_to_french_and_persists_language() {
        let directory = tempfile::tempdir().unwrap();
        load(directory.path()).unwrap();
        database(directory.path())
            .unwrap()
            .execute(
                "INSERT INTO config (id, value) VALUES (1, ?)",
                [r#"{"server":null,"games":[],"theme":"light"}"#],
            )
            .unwrap();

        let mut settings = load(directory.path()).unwrap();
        assert!(matches!(settings.locale, Locale::Fr));
        assert!(matches!(settings.theme, Theme::Light));
        settings.locale = Locale::En;
        save(directory.path(), &settings).unwrap();
        let settings = load(directory.path()).unwrap();
        assert!(matches!(settings.locale, Locale::En));
        assert!(matches!(settings.theme, Theme::Light));
    }
}

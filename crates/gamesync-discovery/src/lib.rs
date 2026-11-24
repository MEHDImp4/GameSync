mod catalog;
mod steam;
#[cfg(windows)]
mod windows;

use serde::Serialize;
use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize)]
pub struct Installation {
    pub store: &'static str,
    pub store_id: String,
    pub title: String,
    pub directory: PathBuf,
    pub executable: Option<PathBuf>,
    pub proton_prefix: Option<PathBuf>,
    pub candidates: Vec<PathBuf>,
}

pub fn discover(catalog: Option<&Path>) -> Result<Vec<Installation>, String> {
    let games = steam::find(&steam_roots());
    #[cfg(windows)]
    let games = {
        let mut games = games;
        games.extend(windows::find());
        games
    };
    let mut games = games;
    if let Some(catalog) = catalog {
        catalog::suggest(catalog, &mut games)?;
    }
    let mut games = deduplicate(games);
    games.sort_by_cached_key(|game| game.title.to_lowercase());
    Ok(games)
}

fn deduplicate(games: Vec<Installation>) -> Vec<Installation> {
    let mut result: Vec<Installation> = Vec::new();
    let mut identifiers = HashSet::new();
    for mut game in games {
        let Ok(directory) = game.directory.canonicalize() else {
            continue;
        };
        if !directory.is_dir() {
            continue;
        }
        let new_identifier = identifiers.insert((game.store, game.store_id.clone()));
        game.directory = directory;
        if let Some(existing) = result
            .iter_mut()
            .find(|item| same_file::is_same_file(&item.directory, &game.directory).unwrap_or(false))
        {
            if existing.executable.is_none() {
                existing.executable = game.executable;
            }
            if existing.proton_prefix.is_none() {
                existing.proton_prefix = game.proton_prefix;
            }
            existing.candidates.extend(game.candidates);
            existing.candidates.sort();
            existing.candidates.dedup();
        } else if new_identifier {
            result.push(game);
        }
    }
    result
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        roots.extend(windows::steam_roots());
        if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
            roots.push(PathBuf::from(program_files).join("Steam"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(data) = env::var_os("XDG_DATA_HOME") {
            roots.push(PathBuf::from(data).join("Steam"));
        }
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            roots.extend([
                home.join(".local/share/Steam"),
                home.join(".steam/steam"),
                home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
            ]);
        }
    }
    roots
}

fn read_small(path: &Path, limit: u64) -> io::Result<String> {
    use io::Read;
    let mut text = String::new();
    let file = fs::File::open(path)?;
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "manifest exceeds size limit",
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_launcher_entries_merge_save_locations() {
        let directory = tempfile::tempdir().unwrap();
        let game = Installation {
            store: "steam",
            store_id: "42".into(),
            title: "Game".into(),
            directory: directory.path().into(),
            executable: None,
            proton_prefix: None,
            candidates: vec![directory.path().join("first")],
        };
        let duplicate = Installation {
            candidates: vec![directory.path().join("second")],
            ..game.clone()
        };
        let games = deduplicate(vec![game, duplicate]);
        assert_eq!(games.len(), 1);
        assert_eq!(
            games[0].candidates,
            vec![
                directory.path().join("first"),
                directory.path().join("second")
            ]
        );
    }

    #[test]
    fn catalog_keeps_save_locations_from_both_launcher_rules() {
        let directory = tempfile::tempdir().unwrap();
        let install = directory.path().join("game");
        fs::create_dir_all(install.join("steam-saves")).unwrap();
        fs::create_dir_all(install.join("gog-saves")).unwrap();
        let catalog = directory.path().join("catalog.yaml");
        fs::write(
            &catalog,
            r#"
Game:
  steam: { id: 42 }
  gog: { id: 73 }
  files:
    <base>/steam-saves:
      tags: [save]
      when: [{store: steam}]
    <base>/gog-saves:
      tags: [save]
      when: [{store: gog}]
"#,
        )
        .unwrap();
        let steam = Installation {
            store: "steam",
            store_id: "42".into(),
            title: "Game".into(),
            directory: install.clone(),
            executable: None,
            proton_prefix: None,
            candidates: vec![],
        };
        let gog = Installation {
            store: "gog",
            store_id: "73".into(),
            ..steam.clone()
        };
        let mut installations = vec![steam, gog];
        catalog::suggest(&catalog, &mut installations).unwrap();
        let games = deduplicate(installations);
        assert_eq!(games.len(), 1);
        assert_eq!(
            games[0].candidates,
            vec![install.join("gog-saves"), install.join("steam-saves")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn duplicate_installations_with_different_path_case() {
        let directory = tempfile::tempdir().unwrap();
        let game_directory = directory.path().join("GameFolder");
        fs::create_dir(&game_directory).unwrap();
        let alternate = directory.path().join("gamefolder");
        let steam = Installation {
            store: "steam",
            store_id: "1".into(),
            title: "Game".into(),
            directory: game_directory,
            executable: None,
            proton_prefix: None,
            candidates: vec![],
        };
        let epic = Installation {
            store: "epic",
            store_id: "game".into(),
            directory: alternate,
            ..steam.clone()
        };
        assert_eq!(deduplicate(vec![steam, epic]).len(), 1);
    }

    #[test]
    fn duplicate_installations_preserve_executable_from_another_launcher() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("game.exe");
        fs::write(&executable, b"test").unwrap();
        let steam = Installation {
            store: "steam",
            store_id: "1".into(),
            title: "Game".into(),
            directory: directory.path().into(),
            executable: None,
            proton_prefix: None,
            candidates: vec![],
        };
        let epic = Installation {
            store: "epic",
            store_id: "game".into(),
            executable: Some(executable.clone()),
            candidates: vec![directory.path().join("saves")],
            ..steam.clone()
        };
        let games = deduplicate(vec![steam, epic]);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].executable.as_ref(), Some(&executable));
        assert_eq!(games[0].candidates, vec![directory.path().join("saves")]);
    }
}

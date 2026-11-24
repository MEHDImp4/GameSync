use super::Installation;
use std::{
    fs,
    path::{Path, PathBuf},
};

// These locations are used by local launcher/emulator profiles. Only propose a
// folder when it contains recognizable save files; never create or change it.
pub fn suggest(games: &mut [Installation]) {
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return;
    };
    for game in games {
        let mut found = find(game, &appdata);
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            found.extend(find_named_locations(game, Path::new(&local)));
        }
        found.extend(find_named_locations(game, &appdata));
        if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            let home = PathBuf::from(home);
            found.extend(find_named_locations(game, &home.join("Documents")));
            found.extend(find_named_locations(game, &home.join("Documents/My Games")));
        }
        // Search only well-known save subfolders inside this installation.
        for directory in [
            "saves",
            "save",
            "savegame",
            "savegames",
            "savedata",
            "Saved/SaveGames",
        ] {
            find_save_directories(
                &game.directory.join(directory),
                false,
                0,
                &mut 2048,
                &mut found,
            );
        }
        for candidate in found {
            if !game
                .candidates
                .iter()
                .any(|existing| same_file::is_same_file(existing, &candidate).unwrap_or(false))
            {
                game.candidates.push(candidate);
            }
        }
    }
}

fn find(game: &Installation, appdata: &Path) -> Vec<PathBuf> {
    let name: String = game
        .title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let mut roots = Vec::new();
    let rdr2 = name == "reddeadredemption2"
        || name == "reddeadredemptionii"
        || (game.store == "steam" && game.store_id == "1174180");
    if rdr2 {
        roots.push((appdata.join(".1911/Red Dead Redemption 2/profile"), true));
        roots.push((appdata.join("Goldberg SocialClub Emu Saves/RDR2"), true));
        roots.push((appdata.join("Goldberg Social Club Emu Saves/RDR2"), true));
    }
    let local_id = steam_id(game);
    if let Some(id) = local_id.as_deref() {
        roots.push((
            appdata
                .join("Goldberg SteamEmu Saves")
                .join(id)
                .join("remote"),
            false,
        ));
    }
    let mut found = Vec::new();
    for (root, red_dead) in roots {
        find_save_directories(&root, red_dead, 0, &mut 2048, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn steam_id(game: &Installation) -> Option<String> {
    if game.store == "steam" && game.store_id.parse::<u32>().is_ok_and(|id| id != 0) {
        return Some(game.store_id.clone());
    }
    // Read only the explicit Steam app ID alongside the executable or in the installation root.
    let mut paths = vec![game.directory.join("steam_appid.txt")];
    if let Some(parent) = game.executable.as_deref().and_then(Path::parent) {
        if parent.starts_with(&game.directory) {
            paths.push(parent.join("steam_appid.txt"));
        }
    }
    paths.into_iter().find_map(|path| {
        if fs::metadata(&path).ok()?.len() > 64 {
            return None;
        }
        let value = fs::read_to_string(path).ok()?;
        let value = value.trim();
        value
            .parse::<u32>()
            .ok()
            .filter(|id| *id != 0)
            .map(|id| id.to_string())
    })
}

fn find_named_locations(game: &Installation, base: &Path) -> Vec<PathBuf> {
    let name = normalized(&game.title);
    if name.len() < 5 {
        return Vec::new();
    }
    let mut found = Vec::new();
    // Search direct game-named directories and one publisher directory level.
    let Ok(entries) = fs::read_dir(base) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok).take(1024) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if normalized(&entry.file_name().to_string_lossy()) == name {
            find_save_directories(&path, false, 0, &mut 2048, &mut found);
        } else if let Ok(children) = fs::read_dir(&path) {
            for child in children.filter_map(Result::ok).take(128) {
                if child
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                    && normalized(&child.file_name().to_string_lossy()) == name
                {
                    find_save_directories(&child.path(), false, 0, &mut 2048, &mut found);
                }
            }
        }
    }
    found
}
fn find_save_directories(
    path: &Path,
    red_dead: bool,
    depth: usize,
    remaining: &mut usize,
    found: &mut Vec<PathBuf>,
) {
    if depth > 4 || *remaining == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut children = Vec::new();
    let mut has_save = false;
    for entry in entries.filter_map(Result::ok) {
        if *remaining == 0 {
            break;
        }
        *remaining -= 1;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            children.push(entry.path());
        } else if kind.is_file() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            has_save |= if red_dead {
                name.starts_with("srdr") || name.starts_with("rdr2save")
            } else {
                name.ends_with(".sav")
                    || name.ends_with(".save")
                    || name.ends_with(".savegame")
                    || name.ends_with(".sl2")
            };
        }
    }
    if has_save {
        found.push(path.to_owned());
    }
    for child in children {
        find_save_directories(&child, red_dead, depth + 1, remaining, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn game(title: &str, store: &'static str, id: &str, root: &Path) -> Installation {
        Installation {
            store,
            store_id: id.into(),
            title: title.into(),
            directory: root.into(),
            executable: None,
            proton_prefix: None,
            candidates: vec![],
        }
    }
    #[test]
    fn finds_red_dead_razor_and_goldberg_saves_without_taking_settings() {
        let temp = tempfile::tempdir().unwrap();
        let razor = temp.path().join(".1911/Red Dead Redemption 2/profile");
        let goldberg = temp.path().join("Goldberg SocialClub Emu Saves/RDR2/1234");
        fs::create_dir_all(&razor).unwrap();
        fs::create_dir_all(&goldberg).unwrap();
        fs::write(razor.join("SRDR30015"), b"save").unwrap();
        fs::write(goldberg.join("SRDR30000"), b"save").unwrap();
        let found = find(
            &game("Red Dead Redemption 2", "folder", "x", temp.path()),
            temp.path(),
        );
        assert_eq!(found, vec![razor, goldberg]);
        assert!(find(&game("Other Game", "folder", "x", temp.path()), temp.path()).is_empty());
    }
    #[test]
    fn discovers_local_steam_id_without_store_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Game");
        let saves = temp.path().join("Goldberg SteamEmu Saves/73/remote");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&saves).unwrap();
        fs::write(install.join("steam_appid.txt"), "73\n").unwrap();
        fs::write(saves.join("slot.sav"), b"save").unwrap();
        let item = game("Example Game", "folder", "unknown", &install);
        assert_eq!(find(&item, temp.path()), vec![saves]);
    }

    #[test]
    fn only_matches_exact_game_directory_names() {
        let temp = tempfile::tempdir().unwrap();
        let exact = temp.path().join("Publisher/Example Game/Saves");
        let other = temp.path().join("Publisher/Example Game 2/Saves");
        fs::create_dir_all(&exact).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(exact.join("slot.sav"), b"save").unwrap();
        fs::write(other.join("slot.sav"), b"other").unwrap();
        assert_eq!(
            find_named_locations(
                &game("Example Game", "folder", "x", temp.path()),
                temp.path()
            ),
            vec![exact]
        );
    }
    #[test]
    fn finds_only_save_files_in_the_matching_steam_id() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Goldberg SteamEmu Saves/42/remote/user");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("settings.ini"), b"settings").unwrap();
        let game = game("Test", "steam", "42", temp.path());
        assert!(find(&game, temp.path()).is_empty());
        fs::write(folder.join("slot.sav"), b"save").unwrap();
        assert_eq!(find(&game, temp.path()), vec![folder]);
    }
}

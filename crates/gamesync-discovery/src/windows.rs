use super::{Installation, read_small};
use serde::Deserialize;
use std::{env, fs, path::PathBuf};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY},
};

pub fn steam_roots() -> Vec<PathBuf> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam");
    key.and_then(|key| key.get_value::<String, _>("SteamPath"))
        .ok()
        .map(PathBuf::from)
        .into_iter()
        .collect()
}

pub fn find() -> Vec<Installation> {
    let mut games = epic();
    let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        let Ok(root) = machine.open_subkey_with_flags("SOFTWARE\\GOG.com\\Games", KEY_READ | view)
        else {
            continue;
        };
        for id in root.enum_keys().filter_map(Result::ok) {
            let Ok(game) = root.open_subkey(&id) else {
                continue;
            };
            let Ok(title) = game.get_value::<String, _>("gameName") else {
                continue;
            };
            let Ok(directory) = game.get_value::<String, _>("path") else {
                continue;
            };
            let directory = PathBuf::from(directory);
            let executable = game
                .get_value::<String, _>("exe")
                .ok()
                .map(|path| directory.join(path))
                .filter(|path| path.is_file());
            games.push(Installation {
                store: "gog",
                store_id: id,
                title,
                directory,
                executable,
                proton_prefix: None,
                candidates: Vec::new(),
            });
        }
    }
    games
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Epic {
    display_name: String,
    app_name: String,
    install_location: PathBuf,
    launch_executable: Option<PathBuf>,
}

fn epic() -> Vec<Installation> {
    let Some(data) = env::var_os("ProgramData").map(PathBuf::from) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(data.join("Epic/EpicGamesLauncher/Data/Manifests")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            if entry.path().extension()?.to_str()? != "item" {
                return None;
            }
            let game: Epic =
                serde_json::from_str(&read_small(&entry.path(), 1024 * 1024).ok()?).ok()?;
            let executable = game
                .launch_executable
                .map(|file| game.install_location.join(file))
                .filter(|file| file.is_file());
            Some(Installation {
                store: "epic",
                store_id: game.app_name,
                title: game.display_name,
                directory: game.install_location,
                executable,
                proton_prefix: None,
                candidates: Vec::new(),
            })
        })
        .collect()
}

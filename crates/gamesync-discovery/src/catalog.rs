use super::{Installation, read_small};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
};

#[derive(Default, Deserialize)]
struct Game {
    steam: Option<StoreId>,
    gog: Option<StoreId>,
    alias: Option<String>,
    #[serde(rename = "installDir", default)]
    install_dir: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    files: BTreeMap<String, Rule>,
}

#[derive(Deserialize)]
struct StoreId {
    id: Option<u64>,
}

#[derive(Deserialize)]
struct Rule {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    when: Vec<Constraint>,
}

#[derive(Deserialize)]
struct Constraint {
    os: Option<String>,
    store: Option<String>,
}

pub fn suggest(path: &Path, installations: &mut [Installation]) -> Result<(), String> {
    let text = read_small(path, 32 * 1024 * 1024)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let games: BTreeMap<String, Game> =
        serde_yaml::from_str(&text).map_err(|error| error.to_string())?;
    let mut names = BTreeMap::<String, Option<&Game>>::new();
    for (title, game) in &games {
        for name in std::iter::once(title).chain(game.install_dir.keys()) {
            let key = normalize_name(name);
            if key.is_empty() {
                continue;
            }
            names
                .entry(key)
                .and_modify(|existing| {
                    if existing.is_some_and(|other| !std::ptr::eq(other, game)) {
                        *existing = None;
                    }
                })
                .or_insert(Some(game));
        }
    }
    for installation in installations {
        let id = installation.store_id.parse::<u64>().ok();
        let matching_id = id.and_then(|id| {
            games.values().find(|game| {
                let store = match installation.store {
                    "steam" => &game.steam,
                    "gog" => &game.gog,
                    _ => return false,
                };
                store.as_ref().and_then(|store| store.id) == Some(id)
            })
        });
        let Some(mut game) = matching_id
            .or_else(|| games.get(&installation.title))
            .or_else(|| {
                names
                    .get(&normalize_name(&installation.title))
                    .copied()
                    .flatten()
            })
        else {
            continue;
        };
        if let Some(alias) = &game.alias {
            let Some(target) = games.get(alias) else {
                continue;
            };
            game = target;
        }
        let mut candidates = BTreeSet::new();
        let native = platform_paths(installation);
        let proton = installation.proton_prefix.as_deref().map(|prefix| {
            let mut paths = native.clone();
            windows_paths(
                &mut paths,
                &prefix.join("drive_c/users/steamuser"),
                Some(prefix),
            );
            paths
        });
        for (template, rule) in &game.files {
            if !rule.tags.iter().any(|tag| tag == "save") {
                continue;
            }
            add_rule(
                &mut candidates,
                template,
                rule,
                installation.store,
                env::consts::OS,
                &native,
            );
            if let Some(proton) = &proton {
                add_rule(
                    &mut candidates,
                    template,
                    rule,
                    installation.store,
                    "windows",
                    proton,
                );
            }
        }
        let mut paths = Vec::<(PathBuf, PathBuf)>::new();
        for candidate in candidates {
            if let Ok(resolved) = candidate.canonicalize() {
                if paths.iter().any(|(_, parent)| resolved.starts_with(parent)) {
                    continue;
                }
                paths.retain(|(_, other)| !other.starts_with(&resolved));
                paths.push((candidate, resolved));
            }
        }
        installation.candidates = paths.into_iter().map(|(path, _)| path).collect();
    }
    Ok(())
}

fn normalize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

type Paths = BTreeMap<&'static str, PathBuf>;

fn environment_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn platform_paths(installation: &Installation) -> Paths {
    let mut paths = Paths::from([("<base>", installation.directory.clone())]);
    if let Some(root) = installation.directory.parent() {
        paths.insert("<root>", root.to_owned());
    }
    if let Some(home) = environment_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        paths.insert("<home>", home.clone());
        if cfg!(windows) {
            windows_paths(&mut paths, &home, None);
        } else {
            paths.insert(
                "<xdgData>",
                environment_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            );
            paths.insert(
                "<xdgConfig>",
                environment_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            );
        }
    }
    paths
}

fn windows_paths(paths: &mut Paths, home: &Path, proton: Option<&Path>) {
    paths.insert("<home>", home.to_owned());
    paths.insert("<winDocuments>", home.join("Documents"));
    paths.insert("<winLocalAppDataLow>", home.join("AppData/LocalLow"));
    let roaming = home.join("AppData/Roaming");
    let local = home.join("AppData/Local");
    let (appdata, local_data) = if let Some(prefix) = proton {
        paths.insert("<winProgramData>", prefix.join("drive_c/ProgramData"));
        paths.insert("<winPublic>", prefix.join("drive_c/users/Public"));
        (roaming, local)
    } else {
        for (placeholder, variable) in [
            ("<winProgramData>", "PROGRAMDATA"),
            ("<winPublic>", "PUBLIC"),
        ] {
            if let Some(value) = environment_path(variable) {
                paths.insert(placeholder, value);
            }
        }
        (
            environment_path("APPDATA").unwrap_or(roaming),
            environment_path("LOCALAPPDATA").unwrap_or(local),
        )
    };
    paths.insert("<winAppData>", appdata);
    paths.insert("<winLocalAppData>", local_data);
}

fn add_rule(
    candidates: &mut BTreeSet<PathBuf>,
    template: &str,
    rule: &Rule,
    store: &str,
    os: &str,
    paths: &Paths,
) {
    if !rule.when.is_empty()
        && !rule.when.iter().any(|constraint| {
            constraint.os.as_deref().is_none_or(|value| value == os)
                && constraint
                    .store
                    .as_deref()
                    .is_none_or(|value| value == store)
        })
    {
        return;
    }
    let Some((placeholder, suffix)) = template.split_once('/') else {
        return;
    };
    let Some(base) = paths.get(placeholder).and_then(|path| path.to_str()) else {
        return;
    };
    // A store user ID denotes a single profile directory.
    // Unknown placeholders remain unsupported.
    let suffix = suffix.replace("<storeUserId>", "*");
    if suffix.is_empty()
        || suffix.contains(['<', '>', '\\', ':'])
        || suffix
            .split('/')
            .any(|part| matches!(part, "" | "." | ".." | "**"))
    {
        return;
    }
    let pattern = format!("{}/{}", glob::Pattern::escape(base), suffix);
    let options = glob::MatchOptions {
        case_sensitive: !cfg!(windows),
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    if let Ok(matches) = glob::glob_with(&pattern, options) {
        candidates.extend(
            matches
                .take(64)
                .filter_map(Result::ok)
                .filter(|path| path.is_dir()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolves_rockstar_profile_placeholder_without_expanding_other_variables() {
        let temp = tempfile::tempdir().unwrap();
        let profiles = temp
            .path()
            .join("Rockstar Games/Red Dead Redemption 2/Profiles");
        let profile = profiles.join("B6296283");
        fs::create_dir_all(&profile).unwrap();
        let paths = Paths::from([("<winDocuments>", temp.path().to_owned())]);
        let rule = Rule {
            tags: vec!["save".into()],
            when: vec![],
        };
        let mut found = BTreeSet::new();
        add_rule(
            &mut found,
            "<winDocuments>/Rockstar Games/Red Dead Redemption 2/Profiles/<storeUserId>",
            &rule,
            "steam",
            "windows",
            &paths,
        );
        assert_eq!(found, BTreeSet::from([profile]));
        found.clear();
        add_rule(
            &mut found,
            "<winDocuments>/Rockstar Games/Red Dead Redemption 2/Profiles/<unknown>",
            &rule,
            "steam",
            "windows",
            &paths,
        );
        assert!(found.is_empty());
    }
    #[test]
    fn matches_install_directory_name_and_finds_saves() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("GameFolder");
        let saves = install.join("Saves");
        fs::create_dir_all(saves.join("profile")).unwrap();
        let yaml = temp.path().join("manifest.yaml");
        fs::write(
            &yaml,
            r#"
A Game:
  installDir:
    GameFolder: {}
  files:
    <base>/Saves:
      tags: [save]
    <base>/Saves/profile:
      tags: [save]
"#,
        )
        .unwrap();
        let mut game = Installation {
            store: "folder",
            store_id: "custom".into(),
            title: "game folder".into(),
            directory: install,
            executable: None,
            proton_prefix: None,
            candidates: vec![],
        };
        suggest(&yaml, std::slice::from_mut(&mut game)).unwrap();
        assert_eq!(game.candidates, vec![saves]);
    }
    #[test]
    fn matches_store_id_and_resolves_proton_save_directory() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("pfx");
        let saves = prefix.join("drive_c/users/steamuser/Saved Games/Studio/Game");
        fs::create_dir_all(&saves).unwrap();
        fs::create_dir_all(prefix.join("drive_c/users/steamuser/Saved Games/Studio/Settings"))
            .unwrap();
        let yaml = temp.path().join("manifest.yaml");
        fs::write(
            &yaml,
            r#"
Canonical Name:
  steam:
    id: 42
  files:
    <home>/Saved Games/Studio/G*:
      tags: [save]
      when: [{os: windows, store: steam}]
    <home>/Saved Games/Studio/Settings:
      tags: [config]
      when: [{os: windows, store: steam}]
"#,
        )
        .unwrap();
        let mut install = Installation {
            store: "steam",
            store_id: "42".into(),
            title: "Localized name".into(),
            directory: temp.path().join("game"),
            executable: None,
            proton_prefix: Some(prefix),
            candidates: Vec::new(),
        };
        suggest(&yaml, std::slice::from_mut(&mut install)).unwrap();
        assert_eq!(install.candidates, vec![saves]);
    }
}

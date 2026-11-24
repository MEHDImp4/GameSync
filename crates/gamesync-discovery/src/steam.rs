use super::{Installation, read_small};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(untagged)]
enum Library {
    Modern { path: PathBuf },
    Legacy(PathBuf),
}

#[derive(Deserialize)]
struct App {
    appid: String,
    name: String,
    installdir: String,
}

pub fn find(roots: &[PathBuf]) -> Vec<Installation> {
    let mut visited = HashSet::new();
    let mut games = Vec::new();
    for root in roots {
        let mut libraries = vec![root.clone()];
        for location in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            let Ok(contents) = read_small(&root.join(location), 4 * 1024 * 1024) else {
                continue;
            };
            if let Ok(metadata) = keyvalues_serde::from_str::<BTreeMap<String, Library>>(&contents)
            {
                libraries.extend(metadata.into_iter().filter_map(|(slot, library)| {
                    if slot.parse::<u32>().is_err() {
                        return None;
                    }
                    let path = match library {
                        Library::Modern { path } | Library::Legacy(path) => path,
                    };
                    path.is_absolute().then_some(path)
                }));
                break;
            }
        }
        for library in libraries {
            if let Ok(root) = library.canonicalize()
                && visited.insert(root.clone())
            {
                games.extend(read_library(&root));
            }
        }
    }
    games
}

fn read_library(library: &Path) -> Vec<Installation> {
    let steamapps = library.join("steamapps");
    let Ok(entries) = fs::read_dir(&steamapps) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let id = name
                .to_str()?
                .strip_prefix("appmanifest_")?
                .strip_suffix(".acf")?;
            id.parse::<u32>().ok()?;
            let contents = read_small(&entry.path(), 1024 * 1024).ok()?;
            let metadata: App = keyvalues_serde::from_str(&contents).ok()?;
            if metadata.appid != id || metadata.name.trim().is_empty() {
                return None;
            }
            let component = Path::new(&metadata.installdir);
            if !matches!(
                component.components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            ) {
                return None;
            }
            let directory = steamapps.join("common").join(component);
            if !directory.is_dir() {
                return None;
            }
            let prefix = steamapps.join("compatdata").join(id).join("pfx");
            Some(Installation {
                store: "steam",
                store_id: id.into(),
                title: metadata.name,
                directory,
                executable: None,
                proton_prefix: prefix.is_dir().then_some(prefix),
                candidates: Vec::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_secondary_library_and_proton_prefix() {
        let primary = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::create_dir_all(primary.path().join("steamapps")).unwrap();
        fs::create_dir_all(second.path().join("steamapps/common/Game")).unwrap();
        fs::create_dir_all(second.path().join("steamapps/compatdata/42/pfx")).unwrap();
        let secondary = second.path().display().to_string().replace('\\', "\\\\");
        fs::write(
            primary.path().join("steamapps/libraryfolders.vdf"),
            format!(
                r#""libraryfolders" {{ "1" {{ "path" "{secondary}" "apps" {{ "42" "1" }} }} }}"#
            ),
        )
        .unwrap();
        fs::write(
            second.path().join("steamapps/appmanifest_42.acf"),
            r#""AppState" { "appid" "42" "name" "Test Game" "installdir" "Game" }"#,
        )
        .unwrap();
        let found = find(&[primary.path().into()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].store_id, "42");
        assert!(found[0].proton_prefix.is_some());
    }
}

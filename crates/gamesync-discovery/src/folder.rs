use super::Installation;
use std::{
    fs,
    path::{Path, PathBuf},
};

// Limit traversal so a broad folder selection cannot scan an entire disk indefinitely.
const MAX_DEPTH: usize = 4;
const MAX_ENTRIES: usize = 20_000;

pub fn find(root: &Path) -> Result<Vec<Installation>, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    if !root.is_dir() {
        return Err("Le chemin choisi n’est pas un dossier.".into());
    }
    let mut remaining = MAX_ENTRIES;
    let mut directories = fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            (kind.is_dir() && !kind.is_symlink()).then(|| entry.path())
        })
        .collect::<Vec<_>>();
    directories.sort();
    let mut games = Vec::new();
    // A selected folder may itself be a game installation.
    if let Some(exe) = find_executable(&root, 0, &mut remaining, false) {
        games.push(installation(&root, exe));
    }
    for directory in directories {
        if remaining == 0 {
            return Err("Dossier trop volumineux : choisissez un dossier plus précis.".into());
        }
        if let Some(exe) = find_executable(&directory, 0, &mut remaining, true) {
            games.push(installation(&directory, exe));
        }
    }
    Ok(games)
}

fn installation(directory: &Path, executable: PathBuf) -> Installation {
    Installation {
        store: "folder",
        store_id: directory.to_string_lossy().into_owned(),
        title: directory
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(['_', '.'], " "),
        directory: directory.to_path_buf(),
        executable: Some(executable),
        proton_prefix: None,
        candidates: vec![],
    }
}

fn find_executable(
    directory: &Path,
    depth: usize,
    remaining: &mut usize,
    recursive: bool,
) -> Option<PathBuf> {
    if depth > MAX_DEPTH || *remaining == 0 {
        return None;
    }
    let mut entries = fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    let mut files = Vec::new();
    let mut children = Vec::new();
    for entry in entries {
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
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if kind.is_dir()
            && recursive
            && ![
                "redist",
                "_commonredist",
                "support",
                "directx",
                "vcredist",
                "uninstall",
                "crashreporter",
            ]
            .contains(&name.as_str())
        {
            children.push(entry.path());
        } else if kind.is_file() && is_game_executable(&entry.path(), &name) {
            files.push(entry.path());
        }
    }
    let folder = directory
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    files.sort_by_key(|path| {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        (!name.eq(&folder), name.contains("launcher"), path.clone())
    });
    files.into_iter().next().or_else(|| {
        children
            .into_iter()
            .find_map(|child| find_executable(&child, depth + 1, remaining, true))
    })
}

fn is_game_executable(path: &Path, name: &str) -> bool {
    if [
        "unins",
        "uninstall",
        "setup",
        "install",
        "crash",
        "report",
        "updater",
        "update",
        "helper",
        "vc_redist",
        "dxsetup",
        "unitycrashhandler",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    {
        return false;
    }
    if name.ends_with(".exe") {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path.extension().is_none()
            && fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_games_in_nested_folders_and_ignores_installers() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("My Game/Binaries/Win64")).unwrap();
        fs::write(root.path().join("My Game/Binaries/Win64/MyGame.exe"), b"").unwrap();
        fs::create_dir_all(root.path().join("Other/_CommonRedist")).unwrap();
        fs::write(root.path().join("Other/_CommonRedist/setup.exe"), b"").unwrap();
        let games = find(root.path()).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "My Game");
        assert!(
            games[0]
                .executable
                .as_ref()
                .unwrap()
                .ends_with("MyGame.exe")
        );
    }
    #[test]
    fn supports_a_game_folder_itself() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("game.exe"), b"").unwrap();
        assert_eq!(find(root.path()).unwrap().len(), 1);
    }
}

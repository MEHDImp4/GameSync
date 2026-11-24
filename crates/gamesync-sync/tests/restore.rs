use gamesync_storage::ObjectStore;
use gamesync_sync::{ApplyError, Restore, capture};
use std::{cell::Cell, fs, io::Read};
use uuid::Uuid;

#[test]
fn interrupted_restore_preserves_old_objects_and_refuses_unexpected_edits() {
    let workspace = tempfile::tempdir().unwrap();
    let local = workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    let state = workspace.path().join("state");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&incoming).unwrap();
    let store = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let root = Uuid::new_v4();
    let roots = [(root, local.clone())];
    fs::write(local.join("a"), b"old a").unwrap();
    fs::write(local.join("b"), b"old b").unwrap();
    fs::write(incoming.join("a"), b"new a").unwrap();
    fs::write(incoming.join("c"), b"new c").unwrap();
    let old = gamesync_sync::scan(&roots).unwrap();
    let target = capture(&[(root, incoming)], &store).unwrap();

    let restore = Restore::new(&state, &store);
    let checks = Cell::new(0);
    let stopped = restore.apply(&roots, &old, &target, || {
        checks.set(checks.get() + 1);
        checks.get() <= 2
    });
    assert!(matches!(stopped, Err(ApplyError::GameActive)));
    assert_eq!(fs::read(local.join("a")).unwrap(), b"new a");

    fs::write(local.join("b"), b"independent play").unwrap();
    let restore = Restore::new(&state, &store);
    assert!(matches!(
        restore.recover(&roots, || true),
        Err(ApplyError::Changed(_))
    ));
    assert_eq!(fs::read(local.join("b")).unwrap(), b"independent play");
    fs::write(local.join("b"), b"old b").unwrap();
    assert!(restore.recover(&roots, || true).unwrap());
    assert!(!restore.recover(&roots, || true).unwrap());
    assert_eq!(gamesync_sync::scan(&roots).unwrap(), target);
    let mut bytes = Vec::new();
    let replaced = &old.entries()[0];
    store
        .open_verified(&replaced.digest, replaced.size)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"old a");
}

#[test]
fn refuses_stale_manifest_before_replacing_any_save_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("game");
    fs::create_dir(&root).unwrap();
    let save = root.join("save");
    let roots = [(Uuid::new_v4(), root)];
    let base = gamesync_sync::scan(&roots).unwrap();
    let store = ObjectStore::open(directory.path().join("objects")).unwrap();
    fs::write(&save, b"new progress").unwrap();
    let journal = directory.path().join("journal");
    let result = Restore::new(&journal, &store).apply(&roots, &base, &base, || true);
    assert!(matches!(result, Err(ApplyError::Changed(_))));
    assert!(!journal.join("apply.json").exists());
    assert_eq!(fs::read(save).unwrap(), b"new progress");
}

#[cfg(target_os = "linux")]
#[test]
fn refuses_redirection_after_staging_a_restore() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let local = workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    let outside = workspace.path().join("outside");
    for directory in [&local.join("profile"), &incoming.join("profile"), &outside] {
        fs::create_dir_all(directory).unwrap();
    }
    let objects = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let root = Uuid::new_v4();
    let roots = [(root, local.clone())];
    fs::write(local.join("profile/save"), b"before").unwrap();
    fs::write(outside.join("save"), b"before").unwrap();
    fs::write(incoming.join("profile/save"), b"remote").unwrap();
    let local_manifest = gamesync_sync::scan(&roots).unwrap();
    let incoming_manifest = capture(&[(root, incoming)], &objects).unwrap();
    let state = workspace.path().join("state");
    let calls = Cell::new(0);
    let result =
        Restore::new(&state, &objects).apply(&roots, &local_manifest, &incoming_manifest, || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                fs::rename(local.join("profile"), local.join("original-profile")).unwrap();
                symlink(&outside, local.join("profile")).unwrap();
            }
            true
        });
    assert!(result.is_err());
    assert_eq!(fs::read(outside.join("save")).unwrap(), b"before");
    assert_eq!(
        fs::read(local.join("original-profile/save")).unwrap(),
        b"before"
    );
    assert_eq!(
        fs::read_dir(local.join("original-profile"))
            .unwrap()
            .count(),
        1
    );
    assert!(gamesync_sync::scan(&[(root, local.join("profile"))]).is_err());
}

#[test]
fn restore_child_exits_after_staging() {
    let Some(workspace) =
        std::env::var_os("GAMESYNC_TEST_RESTORE_WORKSPACE").map(std::path::PathBuf::from)
    else {
        return;
    };
    let id = std::env::var("GAMESYNC_TEST_RESTORE_ROOT")
        .unwrap()
        .parse()
        .unwrap();
    let local = workspace.join("local");
    let roots = [(id, local.clone())];
    let objects = ObjectStore::open(workspace.join("objects")).unwrap();
    let before = gamesync_sync::scan(&roots).unwrap();
    let target = capture(&[(id, workspace.join("incoming"))], &objects).unwrap();
    let calls = Cell::new(0);
    let result =
        Restore::new(&workspace.join("state"), &objects).apply(&roots, &before, &target, || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                // Exit without destructors, as if the process stopped during staging.
                std::process::exit(71);
            }
            true
        });
    panic!("child unexpectedly survived staging: {result:?}");
}

#[test]
fn restart_recovers_verified_partial_stage_and_preserves_unknown_bytes() {
    let workspace = tempfile::tempdir().unwrap();
    let local = workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&incoming).unwrap();
    fs::write(local.join("save"), b"old").unwrap();
    fs::write(incoming.join("save"), b"incoming content").unwrap();
    let root = Uuid::new_v4();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "restore_child_exits_after_staging"])
        .env("GAMESYNC_TEST_RESTORE_WORKSPACE", workspace.path())
        .env("GAMESYNC_TEST_RESTORE_ROOT", root.to_string())
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(71),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    assert_eq!(fs::read(local.join("save")).unwrap(), b"old");
    let stage = local
        .read_dir()
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".gamesync-")
        })
        .unwrap()
        .path();
    assert_eq!(fs::read(&stage).unwrap(), b"incoming content");
    let objects = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let state = workspace.path().join("state");
    let restore = Restore::new(&state, &objects);
    let roots = [(root, local.clone())];

    fs::write(&stage, b"owner's edit").unwrap();
    assert!(matches!(
        restore.recover(&roots, || true),
        Err(ApplyError::Changed(_))
    ));
    assert_eq!(fs::read(&stage).unwrap(), b"owner's edit");
    assert_eq!(fs::read(local.join("save")).unwrap(), b"old");

    fs::write(&stage, b"incoming").unwrap();
    assert!(matches!(
        restore.recover(&roots, || false),
        Err(ApplyError::GameActive)
    ));
    assert!(stage.is_file());
    assert!(restore.recover(&roots, || true).unwrap());
    assert_eq!(fs::read(local.join("save")).unwrap(), b"incoming content");
    assert!(!stage.exists());
    assert!(!state.join("apply.json").exists());
}

#[cfg(windows)]
#[test]
fn windows_holds_parent_directory_during_save_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let local = workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    fs::create_dir_all(local.join("profile")).unwrap();
    fs::create_dir_all(incoming.join("profile")).unwrap();
    fs::write(local.join("profile/save"), b"before").unwrap();
    fs::write(incoming.join("profile/save"), b"after").unwrap();
    let root = Uuid::new_v4();
    let roots = [(root, local.clone())];
    let objects = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let before = gamesync_sync::scan(&roots).unwrap();
    let target = capture(&[(root, incoming)], &objects).unwrap();
    let checks = Cell::new(0);
    Restore::new(&workspace.path().join("state"), &objects)
        .apply(&roots, &before, &target, || {
            checks.set(checks.get() + 1);
            if checks.get() == 2 {
                assert!(fs::rename(local.join("profile"), local.join("elsewhere")).is_err());
            }
            true
        })
        .unwrap();
    assert_eq!(fs::read(local.join("profile/save")).unwrap(), b"after");
}

#[cfg(windows)]
#[test]
fn windows_retries_restore_after_game_releases_locked_save() {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    let workspace = tempfile::tempdir().unwrap();
    let local = workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&incoming).unwrap();
    let save = local.join("progress.sav");
    fs::write(&save, b"local progress").unwrap();
    fs::write(incoming.join("progress.sav"), b"remote progress").unwrap();

    let root = Uuid::new_v4();
    let roots = [(root, local)];
    let objects = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let state = workspace.path().join("state");
    let restore = Restore::new(&state, &objects);
    let before = gamesync_sync::scan(&roots).unwrap();
    let target = capture(&[(root, incoming)], &objects).unwrap();
    // Allow GameSync to read the file while the game denies deletion and replacement.
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&save)
        .unwrap();

    let result = restore.apply(&roots, &before, &target, || true);
    assert!(matches!(result, Err(ApplyError::Io(_))));
    assert_eq!(fs::read(&save).unwrap(), b"local progress");
    assert!(state.join("apply.json").is_file());
    drop(lock);

    assert!(restore.recover(&roots, || true).unwrap());
    assert_eq!(fs::read(&save).unwrap(), b"remote progress");
    assert!(!state.join("apply.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an isolated 1 MiB tmpfs at GAMESYNC_TEST_LIMITED_VOLUME"]
fn linux_restore_retries_after_volume_runs_out_of_space() {
    let limited_volume = std::env::var_os("GAMESYNC_TEST_LIMITED_VOLUME").unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let limited_workspace = tempfile::tempdir_in(limited_volume).unwrap();
    let local = limited_workspace.path().join("local");
    let incoming = workspace.path().join("incoming");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&incoming).unwrap();
    let save = local.join("progress.sav");
    let new_save = vec![0x7A; 768 * 1024];
    fs::write(&save, b"local progress").unwrap();
    fs::write(incoming.join("progress.sav"), &new_save).unwrap();
    let filler = limited_workspace.path().join("filler");
    fs::write(&filler, vec![0xFF; 512 * 1024]).unwrap();

    let root = Uuid::new_v4();
    let roots = [(root, local.clone())];
    let objects = ObjectStore::open(workspace.path().join("objects")).unwrap();
    let state = workspace.path().join("state");
    let restore = Restore::new(&state, &objects);
    let before = gamesync_sync::scan(&roots).unwrap();
    let target = capture(&[(root, incoming)], &objects).unwrap();

    let result = restore.apply(&roots, &before, &target, || true);
    assert!(
        matches!(&result, Err(ApplyError::Io(error)) if error.kind() == std::io::ErrorKind::StorageFull),
        "{result:?}"
    );
    assert_eq!(fs::read(&save).unwrap(), b"local progress");
    assert_eq!(fs::read_dir(&local).unwrap().count(), 1);
    assert!(state.join("apply.json").is_file());

    fs::remove_file(filler).unwrap();
    assert!(restore.recover(&roots, || true).unwrap());
    assert_eq!(fs::read(&save).unwrap(), new_save);
    assert!(!state.join("apply.json").exists());
}

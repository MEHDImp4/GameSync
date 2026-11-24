use gamesync_core::{Decision, decide, retain_revision, validate_path};
use gamesync_sync::scan;
use std::fs;
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn two_machines_detect_independent_edits_and_convergence() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let root_id = Uuid::new_v4();
    let root_a = [(root_id, a.path().to_owned())];
    let root_b = [(root_id, b.path().to_owned())];

    fs::write(a.path().join("partie.sav"), b"original").unwrap();
    let base = scan(&root_a).unwrap();
    assert_eq!(
        base.entries()[0].digest,
        blake3::hash(b"original").to_hex().as_str()
    );
    assert_eq!(
        decide(None, &scan(&root_b).unwrap(), Some(&base)),
        Decision::Download
    );
    fs::copy(a.path().join("partie.sav"), b.path().join("partie.sav")).unwrap();
    assert_eq!(scan(&root_b).unwrap(), base);

    fs::write(a.path().join("partie.sav"), b"save A").unwrap();
    fs::write(b.path().join("partie.sav"), b"save B").unwrap();
    let local = scan(&root_a).unwrap();
    let remote = scan(&root_b).unwrap();
    assert_eq!(decide(Some(&base), &local, Some(&base)), Decision::Publish);
    assert_eq!(
        decide(Some(&base), &local, Some(&remote)),
        Decision::Conflict
    );
    assert_eq!(decide(Some(&base), &local, Some(&local)), Decision::Adopt);
    assert_eq!(decide(None, &local, Some(&remote)), Decision::Conflict);
}

#[test]
fn distinguishes_deleted_file_from_unavailable_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("saves");
    let roots = [(Uuid::new_v4(), root.clone())];
    fs::create_dir(&root).unwrap();
    fs::write(root.join("save"), b"state").unwrap();
    let base = scan(&roots).unwrap();
    fs::remove_file(root.join("save")).unwrap();
    assert_eq!(
        decide(Some(&base), &scan(&roots).unwrap(), Some(&base)),
        Decision::Publish
    );
    fs::remove_dir(root).unwrap();
    assert!(scan(&roots).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_links_and_case_collisions() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let roots = [(Uuid::new_v4(), dir.path().to_owned())];
    fs::write(dir.path().join("save"), b"state").unwrap();
    symlink("save", dir.path().join("link")).unwrap();
    assert!(scan(&roots).is_err());
    fs::remove_file(dir.path().join("link")).unwrap();
    fs::write(dir.path().join("SAVE"), b"state").unwrap();
    assert!(scan(&roots).is_err());
}

#[test]
fn portable_paths_and_retention() {
    for path in [
        "../save",
        "/save",
        "C:/save",
        "save:stream",
        "dir/LPT¹.sav",
        "a//b",
        "foo.",
    ] {
        assert!(validate_path(path).is_err(), "{path}");
    }
    assert!(validate_path("Profil/Épisode 1.sav").is_ok());
    assert!(retain_revision(19, 90, 20, 30, false));
    assert!(retain_revision(99, 29, 20, 30, false));
    assert!(retain_revision(99, 90, 20, 30, true));
    assert!(!retain_revision(20, 30, 20, 30, false));
}

#[test]
fn snapshot_keeps_bytes_after_local_save_changes() {
    use std::io::Read;

    let local = tempdir().unwrap();
    let data = tempdir().unwrap();
    let store = gamesync_storage::ObjectStore::open(data.path()).unwrap();
    let save = local.path().join("save");
    fs::write(&save, b"old save").unwrap();
    let snapshot =
        gamesync_sync::capture(&[(Uuid::new_v4(), local.path().into())], &store).unwrap();
    fs::write(save, b"new save").unwrap();

    let entry = &snapshot.entries()[0];
    let mut bytes = Vec::new();
    store
        .open_verified(&entry.digest, entry.size)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"old save");
}

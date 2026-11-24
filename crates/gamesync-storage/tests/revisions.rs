use gamesync_core::{FileEntry, Manifest};
use gamesync_storage::{ObjectStore, Publication, Revisions};
use std::{path::Path, sync::Barrier, thread};
use uuid::Uuid;

fn store_manifest(objects: &ObjectStore, root: Uuid, bytes: &[u8]) -> Manifest {
    let digest = blake3::hash(bytes).to_hex().to_string();
    let size = bytes.len() as u64;
    objects.put(&digest, size, bytes).unwrap();
    Manifest::new(vec![FileEntry {
        root,
        path: "save".into(),
        size,
        digest,
    }])
    .unwrap()
}

#[test]
fn concurrent_writes_preserve_variants_and_allow_restoring_previous_content() {
    let data = tempfile::tempdir().unwrap();
    let database = data.path().join("revisions.sqlite");
    let objects_path = data.path().join("objects");
    let objects = ObjectStore::open(&objects_path).unwrap();
    let mut revisions = Revisions::open(&database).unwrap();
    let game = Uuid::new_v4();
    let root = Uuid::new_v4();
    let machine = Uuid::new_v4();
    let original = store_manifest(&objects, root, b"original");
    let manifest_a = store_manifest(&objects, root, b"variant A");
    let manifest_b = store_manifest(&objects, root, b"variant B");
    let Publication::Head(base) = revisions
        .publish(&objects, game, machine, Uuid::new_v4(), None, &original)
        .unwrap()
    else {
        panic!("first revision must become head")
    };

    let barrier = Barrier::new(2);
    let submit = |manifest: &Manifest, db: &Path| {
        let mut revisions = Revisions::open(db).unwrap();
        let objects = ObjectStore::open(&objects_path).unwrap();
        let request = Uuid::new_v4();
        let machine = Uuid::new_v4();
        barrier.wait();
        let result = revisions
            .publish(&objects, game, machine, request, Some(base), manifest)
            .unwrap();
        assert_eq!(
            revisions
                .publish(&objects, game, machine, request, Some(base), manifest)
                .unwrap(),
            result
        );
        result
    };
    let (a, b) = thread::scope(|scope| {
        let a = scope.spawn(|| submit(&manifest_a, &database));
        let b = scope.spawn(|| submit(&manifest_b, &database));
        (a.join().unwrap(), b.join().unwrap())
    });
    let head = match (a, b) {
        (Publication::Head(head), Publication::Conflict(other)) => {
            assert_eq!(revisions.get(game, head).unwrap().manifest, manifest_a);
            assert_eq!(revisions.get(game, other).unwrap().manifest, manifest_b);
            head
        }
        (Publication::Conflict(other), Publication::Head(head)) => {
            assert_eq!(revisions.get(game, head).unwrap().manifest, manifest_b);
            assert_eq!(revisions.get(game, other).unwrap().manifest, manifest_a);
            head
        }
        results => panic!("expected one head and one conflict: {results:?}"),
    };
    let Publication::Head(restored) = revisions
        .publish(
            &objects,
            game,
            machine,
            Uuid::new_v4(),
            Some(head),
            &original,
        )
        .unwrap()
    else {
        panic!("restore must create a new head")
    };
    drop(revisions);
    let reopened = Revisions::open(&database).unwrap();
    assert_eq!(reopened.head(game).unwrap(), Some(restored));
    assert_eq!(reopened.get(game, restored).unwrap().manifest, original);
    assert_eq!(reopened.get(game, base).unwrap().manifest, original);
}

#[test]
fn retention_keeps_heads_and_conflict_bases_and_collects_only_unreferenced_objects() {
    let data = tempfile::tempdir().unwrap();
    let database = data.path().join("revisions.sqlite");
    let objects = ObjectStore::open(data.path().join("objects")).unwrap();
    let mut revisions = Revisions::open(&database).unwrap();
    let game = Uuid::new_v4();
    let machine = Uuid::new_v4();
    let root = Uuid::new_v4();
    let old = store_manifest(&objects, root, b"prunable");
    let base = store_manifest(&objects, root, b"conflict base");
    let head = store_manifest(&objects, root, b"current");
    let conflict = store_manifest(&objects, root, b"conflict content");

    let old_request = Uuid::new_v4();
    let Publication::Head(old_id) = revisions
        .publish(&objects, game, machine, old_request, None, &old)
        .unwrap()
    else {
        panic!()
    };
    let Publication::Head(base_id) = revisions
        .publish(&objects, game, machine, Uuid::new_v4(), Some(old_id), &base)
        .unwrap()
    else {
        panic!()
    };
    let Publication::Head(head_id) = revisions
        .publish(
            &objects,
            game,
            machine,
            Uuid::new_v4(),
            Some(base_id),
            &head,
        )
        .unwrap()
    else {
        panic!()
    };
    let Publication::Conflict(conflict_id) = revisions
        .publish(
            &objects,
            game,
            machine,
            Uuid::new_v4(),
            Some(base_id),
            &conflict,
        )
        .unwrap()
    else {
        panic!()
    };
    let clean = revisions
        .cleanup(&objects, 1, 30, std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(clean.expired_revisions, 0);
    assert_eq!(clean.removed_objects, 0);

    rusqlite::Connection::open(&database)
        .unwrap()
        .execute("UPDATE revisions SET created_at = 1", [])
        .unwrap();
    let clean = revisions
        .cleanup(&objects, 1, 30, std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(clean.expired_revisions, 1);
    assert_eq!(clean.removed_objects, 1);
    assert!(matches!(
        revisions.get(game, old_id),
        Err(gamesync_storage::RevisionError::ExpiredRevision)
    ));
    assert_eq!(
        revisions
            .publish(&objects, game, machine, old_request, None, &old)
            .unwrap(),
        Publication::Head(old_id)
    );
    assert_eq!(revisions.head(game).unwrap(), Some(head_id));
    for (id, manifest) in [(base_id, &base), (head_id, &head), (conflict_id, &conflict)] {
        let file = &manifest.entries()[0];
        assert!(objects.open_verified(&file.digest, file.size).is_ok());
        assert_eq!(&revisions.get(game, id).unwrap().manifest, manifest);
    }
    let request = Uuid::new_v4();
    let resolve = |revisions: &mut Revisions| {
        revisions
            .publish_with_resolution(
                &objects,
                gamesync_storage::PublishInput {
                    game,
                    machine,
                    request,
                    base: Some(head_id),
                    manifest: &head,
                    resolves: Some(conflict_id),
                },
            )
            .unwrap()
    };
    let Publication::Head(resolution) = resolve(&mut revisions) else {
        panic!()
    };
    assert_eq!(resolve(&mut revisions), Publication::Head(resolution));
    assert_eq!(
        revisions.get(game, conflict_id).unwrap().resolved_by,
        Some(resolution)
    );
    revisions
        .cleanup(&objects, 1, 30, std::time::Duration::ZERO)
        .unwrap();
    assert!(revisions.get(game, conflict_id).is_ok());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute("UPDATE revisions SET created_at = 1", [])
        .unwrap();
    revisions
        .cleanup(&objects, 1, 30, std::time::Duration::ZERO)
        .unwrap();
    assert!(matches!(
        revisions.get(game, conflict_id),
        Err(gamesync_storage::RevisionError::ExpiredRevision)
    ));
    assert_eq!(revisions.head(game).unwrap(), Some(resolution));
}

#[test]
fn history_paginates_without_repeating_the_cursor() {
    let data = tempfile::tempdir().unwrap();
    let objects = ObjectStore::open(data.path().join("objects")).unwrap();
    let mut revisions = Revisions::open(data.path().join("revisions.sqlite")).unwrap();
    let game = Uuid::new_v4();
    let machine = Uuid::new_v4();
    let manifest = Manifest::new(Vec::new()).unwrap();
    let mut base = None;
    for _ in 0..51 {
        let Publication::Head(id) = revisions
            .publish(&objects, game, machine, Uuid::new_v4(), base, &manifest)
            .unwrap()
        else {
            panic!("expected a new head");
        };
        base = Some(id);
    }
    let first = revisions.history(game, i64::MAX).unwrap();
    assert_eq!(first.len(), 50);
    let cursor = first.last().unwrap().id;
    let next = revisions.history(game, cursor).unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0].id < cursor);
}

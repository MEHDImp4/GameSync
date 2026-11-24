use gamesync_core::{FileEntry, Manifest};
use gamesync_protocol::Api;
use std::{
    net::TcpListener,
    process::{Child, Command},
    time::Duration,
};
use uuid::Uuid;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn rust_client_pairs_transfers_and_reads_a_revision() {
    let directory = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_gamesync-server"));
    command
        .env("GAMESYNC_DATA", directory.path())
        .env("GAMESYNC_BIND", "127.0.0.1")
        .env("GAMESYNC_PORT", port.to_string())
        .env("GAMESYNC_KEEP_COUNT", "20")
        .env("GAMESYNC_KEEP_DAYS", "30");
    let invite = Command::new(env!("CARGO_BIN_EXE_gamesync-server"))
        .args(["invite", "--admin"])
        .env("GAMESYNC_DATA", directory.path())
        .output()
        .unwrap();
    assert!(invite.status.success());
    let invite = std::str::from_utf8(&invite.stdout).unwrap().trim();
    let _server = Server(command.spawn().unwrap());
    let address = format!("http://127.0.0.1:{port}");
    let probe = reqwest::Client::new();
    for _ in 0..50 {
        if probe.get(format!("{address}/healthz")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let spec = probe
        .get(format!("{address}/openapi.yaml"))
        .send()
        .await
        .unwrap();
    assert!(spec.status().is_success());
    let spec: serde_yaml::Value = serde_yaml::from_str(&spec.text().await.unwrap()).unwrap();
    assert!(spec["paths"]["/v1/status"]["get"].is_mapping());
    assert!(
        !probe
            .get(format!("{address}/v1/status"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let client = Api::new(&address).unwrap();
    let credentials = client.pair(invite, "Test client").await.unwrap();
    let token = credentials.token.clone();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = progress.clone();
    let session = client.authenticate(credentials).with_transfer_options(
        gamesync_protocol::TransferOptions {
            bytes_per_second: std::num::NonZeroU64::new(64),
            observer: Some(std::sync::Arc::new(move |update| {
                events.lock().unwrap().push(update)
            })),
        },
    );
    let source = directory.path().join("source.sav");
    let staged = directory.path().join("staged.sav");
    let bytes = b"saved progress";
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(&staged, &bytes[..3]).unwrap();
    let entry = FileEntry {
        root: Uuid::new_v4(),
        path: "save.sav".into(),
        size: bytes.len() as u64,
        digest: blake3::hash(bytes).to_hex().to_string(),
    };
    let start = std::time::Instant::now();
    session.upload(&entry, &source).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(200));
    let manifest = Manifest::new(vec![entry.clone()]).unwrap();
    let game = Uuid::new_v4();
    let first = session
        .publish(game, Uuid::new_v4(), None, &manifest)
        .await
        .unwrap();
    assert_eq!(session.head(game).await.unwrap(), Some(first.revision));
    for invalid_path in ["../escape", "C:/save", "folder/../../escape"] {
        let payload = serde_json::json!({
            "request": Uuid::new_v4(),
            "base": first.revision,
            "manifest": [{ "root": entry.root, "path": invalid_path, "size": entry.size, "digest": entry.digest }],
            "resolves": null
        });
        let response = probe
            .post(format!("{address}/v1/games/{game}/head"))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success(), "accepted {invalid_path}");
        assert_eq!(session.head(game).await.unwrap(), Some(first.revision));
    }
    assert_eq!(
        session
            .revision(game, first.revision)
            .await
            .unwrap()
            .manifest,
        manifest
    );
    let unauthenticated = probe
        .post(format!("{address}/v1/games/{game}/head"))
        .json(&serde_json::json!({
            "request": Uuid::new_v4(), "base": first.revision,
            "manifest": [], "resolves": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(session.head(game).await.unwrap(), Some(first.revision));

    let missing_digest = blake3::hash(b"object never uploaded").to_hex().to_string();
    let missing_object = serde_json::json!({
        "request": Uuid::new_v4(),
        "base": first.revision,
        "manifest": [{ "root": entry.root, "path": "missing.sav", "size": 21, "digest": missing_digest }],
        "resolves": null
    });
    let response = probe
        .post(format!("{address}/v1/games/{game}/head"))
        .bearer_auth(&token)
        .json(&missing_object)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(session.head(game).await.unwrap(), Some(first.revision));

    let duplicate_case = serde_json::json!({
        "request": Uuid::new_v4(),
        "base": first.revision,
        "manifest": [
            { "root": entry.root, "path": "Save.dat", "size": entry.size, "digest": entry.digest },
            { "root": entry.root, "path": "save.dat", "size": entry.size, "digest": entry.digest }
        ],
        "resolves": null
    });
    let response = probe
        .post(format!("{address}/v1/games/{game}/head"))
        .bearer_auth(&token)
        .json(&duplicate_case)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(session.head(game).await.unwrap(), Some(first.revision));
    assert!(
        session
            .publish(game, Uuid::new_v4(), None, &manifest)
            .await
            .unwrap()
            .conflict
    );
    session.download(&entry, &staged).await.unwrap();
    assert_eq!(std::fs::read(&staged).unwrap(), bytes);
    std::fs::write(&staged, b"bad").unwrap();
    assert!(matches!(
        session.download(&entry, &staged).await,
        Err(gamesync_protocol::ClientError::Integrity)
    ));
    assert_eq!(std::fs::metadata(&staged).unwrap().len(), 0);
    session.download(&entry, &staged).await.unwrap();
    assert_eq!(std::fs::read(&staged).unwrap(), bytes);
    {
        use gamesync_protocol::Direction;
        let events = progress.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event.direction, Direction::Upload)
                    && event.transferred == entry.size)
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.direction, Direction::Download)
                    && event.transferred == 3)
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.direction, Direction::Download)
                    && event.transferred == entry.size)
        );
    }
    let session = session.with_transfer_options(gamesync_protocol::TransferOptions::default());

    use gamesync_sync::{Outcome, Workflow};
    assert!(session.devices().await.unwrap().administrator);
    let second_invite = session.invite_device().await.unwrap();
    let second_api = Api::new(&address).unwrap();
    let second = second_api
        .pair(&second_invite, "Second client")
        .await
        .unwrap();
    let second_token = second.token.clone();
    let second = second_api.authenticate(second);
    let devices = second.devices().await.unwrap();
    assert!(!devices.administrator);
    assert_eq!(devices.items.len(), 2);
    assert!(second.invite_device().await.is_err());
    let status = session.status().await.unwrap();
    assert_eq!((status.keep_count, status.keep_days), (20, 30));
    let unauthorized = probe
        .put(format!("{address}/v1/retention"))
        .header("content-type", "application/json")
        .body(r#"{"keep_count":15,"keep_days":45}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(matches!(
        second.set_retention(15, 45).await,
        Err(gamesync_protocol::ClientError::Status(
            reqwest::StatusCode::FORBIDDEN
        ))
    ));
    assert!(matches!(
        session.set_retention(0, 45).await,
        Err(gamesync_protocol::ClientError::Status(
            reqwest::StatusCode::BAD_REQUEST
        ))
    ));
    session.set_retention(15, 45).await.unwrap();
    let status = second.status().await.unwrap();
    assert_eq!((status.keep_count, status.keep_days), (15, 45));
    assert!(second.revoke_device(session.device()).await.is_err());
    let concurrent_game = Uuid::new_v4();
    let base = session
        .publish(concurrent_game, Uuid::new_v4(), None, &manifest)
        .await
        .unwrap();
    let mut other_entry = entry.clone();
    other_entry.path = "alternate.sav".into();
    let alternate = Manifest::new(vec![other_entry]).unwrap();
    let (a, b) = tokio::join!(
        session.publish(
            concurrent_game,
            Uuid::new_v4(),
            Some(base.revision),
            &manifest
        ),
        second.publish(
            concurrent_game,
            Uuid::new_v4(),
            Some(base.revision),
            &alternate
        ),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.conflict, b.conflict);
    assert_ne!(a.revision, b.revision);
    let head = session.head(concurrent_game).await.unwrap().unwrap();
    assert_eq!(head, if a.conflict { b.revision } else { a.revision });
    assert_eq!(
        session
            .revision(concurrent_game, a.revision)
            .await
            .unwrap()
            .manifest,
        manifest
    );
    assert_eq!(
        session
            .revision(concurrent_game, b.revision)
            .await
            .unwrap()
            .manifest,
        alternate
    );

    let deletion_game = Uuid::new_v4();
    let deletion_root_id = Uuid::new_v4();
    let deletion_a = directory.path().join("deletion-a");
    let deletion_b = directory.path().join("deletion-b");
    std::fs::create_dir(&deletion_a).unwrap();
    std::fs::create_dir(&deletion_b).unwrap();
    std::fs::write(deletion_a.join("keep.sav"), b"keep").unwrap();
    std::fs::write(deletion_a.join("remove.sav"), b"original").unwrap();
    let roots_a = [(deletion_root_id, deletion_a.clone())];
    let roots_b = [(deletion_root_id, deletion_b.clone())];
    let state_a = directory.path().join("deletion-state-a");
    let state_b = directory.path().join("deletion-state-b");
    let deletion_workflow_a = Workflow {
        game: deletion_game,
        roots: &roots_a,
        state_dir: &state_a,
        session: &session,
    };
    let deletion_workflow_b = Workflow {
        game: deletion_game,
        roots: &roots_b,
        state_dir: &state_b,
        session: &second,
    };
    let Outcome::Synchronized(before_deletion) = deletion_workflow_a.run(|| true).await.unwrap()
    else {
        panic!("initial snapshot must publish")
    };
    assert!(matches!(
        deletion_workflow_b.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(
        std::fs::read(deletion_b.join("remove.sav")).unwrap(),
        b"original"
    );
    std::fs::remove_file(deletion_a.join("remove.sav")).unwrap();
    let Outcome::Synchronized(after_deletion) = deletion_workflow_a.run(|| true).await.unwrap()
    else {
        panic!("deletion must publish")
    };
    assert!(matches!(
        deletion_workflow_b.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert!(!deletion_b.join("remove.sav").exists());
    assert_eq!(std::fs::read(deletion_b.join("keep.sav")).unwrap(), b"keep");
    assert!(matches!(
        deletion_workflow_b
            .choose_revision(before_deletion, after_deletion, || true)
            .await
            .unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(
        std::fs::read(deletion_b.join("remove.sav")).unwrap(),
        b"original"
    );
    assert!(matches!(
        deletion_workflow_a.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(
        std::fs::read(deletion_a.join("remove.sav")).unwrap(),
        b"original"
    );
    assert_eq!(std::fs::read(deletion_a.join("keep.sav")).unwrap(), b"keep");
    let current_before_missing = session.head(deletion_game).await.unwrap();
    let moved_root = directory.path().join("deletion-a-offline");
    std::fs::rename(&deletion_a, &moved_root).unwrap();
    assert!(deletion_workflow_a.run(|| true).await.is_err());
    assert_eq!(
        session.head(deletion_game).await.unwrap(),
        current_before_missing
    );
    assert_eq!(
        std::fs::read(moved_root.join("remove.sav")).unwrap(),
        b"original"
    );
    std::fs::rename(&moved_root, &deletion_a).unwrap();
    assert!(matches!(
        deletion_workflow_a.run(|| true).await.unwrap(),
        Outcome::Unchanged
    ));
    let deletion_history = session.history(deletion_game, None).await.unwrap();
    assert!(
        deletion_history
            .iter()
            .any(|revision| revision.id == before_deletion)
    );
    assert!(
        deletion_history
            .iter()
            .any(|revision| revision.id == after_deletion)
    );
    let removed = session
        .revision(deletion_game, after_deletion)
        .await
        .unwrap()
        .manifest;
    assert!(
        removed
            .entries()
            .iter()
            .all(|file| file.path != "remove.sav")
    );
    let original = session
        .revision(deletion_game, before_deletion)
        .await
        .unwrap()
        .manifest;
    assert!(
        original
            .entries()
            .iter()
            .any(|file| file.path == "remove.sav"
                && file.digest == blake3::hash(b"original").to_hex().as_str())
    );

    let first_root = directory.path().join("first-root");
    let second_root = directory.path().join("second-root");
    std::fs::create_dir(&first_root).unwrap();
    std::fs::create_dir(&second_root).unwrap();
    let profile = Uuid::new_v4();
    let profile_a = directory.path().join("profile-a");
    let profile_b = directory.path().join("profile-b");
    std::fs::create_dir(&profile_a).unwrap();
    std::fs::create_dir(&profile_b).unwrap();
    std::fs::write(profile_a.join("profile.sav"), b"profile status").unwrap();
    let first_roots = [(entry.root, first_root.clone()), (profile, profile_a)];
    let second_roots = [
        (entry.root, second_root.clone()),
        (profile, profile_b.clone()),
    ];
    let game = Uuid::new_v4();
    let registration = gamesync_protocol::GameRegistration {
        id: game,
        title: "Test multi-dossier".into(),
        roots: vec![
            gamesync_protocol::GameRoot {
                id: entry.root,
                label: "Progression".into(),
            },
            gamesync_protocol::GameRoot {
                id: profile,
                label: "Profil".into(),
            },
        ],
    };
    session.register_game(&registration).await.unwrap();
    assert_eq!(second.games().await.unwrap(), vec![registration.clone()]);
    second.register_game(&registration).await.unwrap();
    let mut invalid = registration.clone();
    invalid.roots[0].id = Uuid::new_v4();
    assert!(second.register_game(&invalid).await.is_err());
    let mismatched = Manifest::new(vec![FileEntry {
        root: invalid.roots[0].id,
        ..entry.clone()
    }])
    .unwrap();
    assert!(
        second
            .publish(game, Uuid::new_v4(), None, &mismatched)
            .await
            .is_err()
    );
    let first_state = directory.path().join("first-state");
    let second_state = directory.path().join("second-state");
    let first_workflow = Workflow {
        game,
        roots: &first_roots,
        state_dir: &first_state,
        session: &session,
    };
    let second_workflow = Workflow {
        game,
        roots: &second_roots,
        state_dir: &second_state,
        session: &second,
    };
    let first_save = first_root.join("progress.sav");
    let second_save = second_root.join("progress.sav");
    std::fs::write(&first_save, b"save A").unwrap();
    assert!(matches!(
        first_workflow.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(
        second_workflow.run(|| false).await.unwrap(),
        Outcome::Deferred
    );
    assert!(!second_save.exists());
    assert!(matches!(
        second_workflow.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(std::fs::read(&second_save).unwrap(), b"save A");
    assert_eq!(
        std::fs::read(profile_b.join("profile.sav")).unwrap(),
        b"profile status"
    );
    std::fs::write(&second_save, b"save B").unwrap();
    assert!(matches!(
        second_workflow.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    let db_path = directory.path().join("metadata.sqlite");
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute(
            "UPDATE revisions SET created_at = 1 WHERE game = ?",
            [game.to_string()],
        )
        .unwrap();
    session.set_retention(1, 1).await.unwrap();
    let cleanup = Command::new(env!("CARGO_BIN_EXE_gamesync-server"))
        .arg("cleanup")
        .env("GAMESYNC_DATA", directory.path())
        .env("GAMESYNC_KEEP_COUNT", "20")
        .env("GAMESYNC_KEEP_DAYS", "30")
        .output()
        .unwrap();
    assert!(cleanup.status.success());
    assert!(String::from_utf8_lossy(&cleanup.stderr).contains("expired_revisions=1"));
    session.set_retention(15, 45).await.unwrap();

    assert!(matches!(
        first_workflow.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(std::fs::read(&first_save).unwrap(), b"save B");

    std::fs::write(&first_save, b"save AA").unwrap();
    std::fs::write(&second_save, b"save BB").unwrap();
    let Outcome::Synchronized(head) = first_workflow.run(|| true).await.unwrap() else {
        panic!("the first machine must publish");
    };
    let Outcome::Conflict { variant } = second_workflow.run(|| true).await.unwrap() else {
        panic!("the second machine must retain a variant");
    };
    assert_eq!(
        second_workflow.run(|| true).await.unwrap(),
        Outcome::Conflict { variant }
    );
    assert_eq!(std::fs::read(&second_save).unwrap(), b"save BB");
    assert_eq!(second.head(game).await.unwrap(), Some(head));
    let fork = second.revision(game, variant).await.unwrap();
    assert_eq!(
        fork.manifest
            .entries()
            .iter()
            .find(|file| file.root == entry.root)
            .unwrap()
            .digest,
        blake3::hash(b"save BB").to_hex().as_str()
    );
    let history = second.history(game, None).await.unwrap();
    assert_eq!(history[0].id, variant);
    assert!(!history[0].published);
    assert_eq!(history[0].bytes, 21);
    let previous = second.history(game, Some(head)).await.unwrap();
    assert!(previous.iter().all(|item| item.id < head));
    assert!(matches!(
        second_workflow
            .choose_revision(variant, head - 1, || true)
            .await,
        Err(gamesync_sync::WorkflowError::StaleSelection)
    ));
    assert_eq!(std::fs::read(&second_save).unwrap(), b"save BB");
    assert_eq!(second.head(game).await.unwrap(), Some(head));
    assert_eq!(
        second.revision(game, variant).await.unwrap().manifest,
        fork.manifest
    );
    let stale = second
        .publish_resolving(
            game,
            Uuid::new_v4(),
            previous.first().map(|item| item.id),
            &fork.manifest,
            Some(variant),
        )
        .await
        .err()
        .unwrap();
    assert!(stale.is_stale_resolution());
    assert!(
        second
            .revision(game, variant)
            .await
            .unwrap()
            .resolved_by
            .is_none()
    );
    let Outcome::Synchronized(resolved) = second_workflow
        .choose_revision(variant, head, || true)
        .await
        .unwrap()
    else {
        panic!("selected conflict variant must become head");
    };
    assert_ne!(resolved, variant);
    assert_eq!(
        second.revision(game, variant).await.unwrap().resolved_by,
        Some(resolved)
    );
    assert!(
        second
            .history(game, None)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == variant && item.resolved_by == Some(resolved))
    );
    assert_eq!(second.head(game).await.unwrap(), Some(resolved));
    assert!(matches!(
        first_workflow.run(|| true).await.unwrap(),
        Outcome::Synchronized(_)
    ));
    assert_eq!(std::fs::read(&first_save).unwrap(), b"save BB");
    let Outcome::Synchronized(restored) = first_workflow
        .choose_revision(head, resolved, || true)
        .await
        .unwrap()
    else {
        panic!("historical revision must become head");
    };
    assert!(restored > resolved);
    assert_eq!(std::fs::read(&first_save).unwrap(), b"save AA");
    assert_eq!(
        first_state
            .join("before-restore")
            .read_dir()
            .unwrap()
            .count(),
        1
    );
    let replacement_a = directory.path().join("remapped-progress");
    let replacement_b = directory.path().join("remapped-profile");
    std::fs::create_dir(&replacement_a).unwrap();
    std::fs::create_dir(&replacement_b).unwrap();
    let replacement = [
        (entry.root, replacement_a.clone()),
        (profile, replacement_b.clone()),
    ];
    let remapped = Workflow {
        game,
        roots: &replacement,
        state_dir: &first_state,
        session: &session,
    };
    assert_eq!(
        remapped.run(|| true).await.unwrap(),
        Outcome::Synchronized(restored)
    );
    assert_eq!(
        std::fs::read(replacement_a.join("progress.sav")).unwrap(),
        b"save AA"
    );
    assert_eq!(
        std::fs::read(replacement_b.join("profile.sav")).unwrap(),
        b"profile status"
    );
    assert_eq!(session.head(game).await.unwrap(), Some(restored));

    let conflicting_dir = directory.path().join("independent");
    std::fs::create_dir(&conflicting_dir).unwrap();
    std::fs::write(conflicting_dir.join("progress.sav"), b"independent").unwrap();
    let changed_mapping = [
        (entry.root, conflicting_dir.clone()),
        (profile, replacement_b),
    ];
    let changed = Workflow {
        game,
        roots: &changed_mapping,
        state_dir: &first_state,
        session: &session,
    };
    assert!(matches!(
        changed.run(|| true).await.unwrap(),
        Outcome::Conflict { .. }
    ));
    assert_eq!(session.head(game).await.unwrap(), Some(restored));
    assert_eq!(
        std::fs::read(conflicting_dir.join("progress.sav")).unwrap(),
        b"independent"
    );
    let status = session.status().await.unwrap();
    assert_eq!(status.synchronized_games, 4);
    assert!(status.used_bytes > 0);
    assert_eq!(
        status.latest[0].title.as_deref(),
        Some("Test multi-dossier")
    );
    assert_eq!(status.latest[0].revision, restored);
    assert!(status.errors.is_empty());
    let metadata = directory.path().join("metadata.sqlite");
    let backup = metadata.with_extension("test-backup");
    std::fs::rename(&metadata, &backup).unwrap();
    std::fs::create_dir(&metadata).unwrap();
    assert!(session.head(game).await.is_err());
    std::fs::remove_dir(&metadata).unwrap();
    std::fs::rename(backup, &metadata).unwrap();
    let errors = session.status().await.unwrap().errors;
    assert_eq!(errors[0].status, Some(500));
    assert_eq!(errors[0].operation, "GET /v1/games/{game}/head");
    assert!(session.revoke_device(session.device()).await.is_err());
    let transfer = b"revoked mid transfer";
    let transfer_hash = blake3::hash(transfer).to_hex().to_string();
    let transfer_url = format!("{address}/v1/uploads/{transfer_hash}/{}", transfer.len());
    let response = probe
        .put(format!("{transfer_url}/0"))
        .bearer_auth(&second_token)
        .body(transfer[..5].to_vec())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    session.revoke_device(second.device()).await.unwrap();
    let devices = session.devices().await.unwrap();
    assert!(
        devices
            .items
            .iter()
            .any(|item| item.id == second.device() && item.revoked)
    );
    assert!(second.head(game).await.is_err());
    let response = probe
        .put(format!("{transfer_url}/5"))
        .bearer_auth(&second_token)
        .body(transfer[5..].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = probe
        .get(format!(
            "{address}/v1/objects/{transfer_hash}/{}",
            transfer.len()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let pair_url = format!("{address}/v1/pair");
    for _ in 0..28 {
        let response = probe.post(&pair_url).body("invalid").send().await.unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
    let throttled = probe.post(pair_url).body("invalid").send().await.unwrap();
    assert_eq!(throttled.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(throttled.headers().get("retry-after").unwrap(), "60");
    assert_eq!(
        probe
            .get(format!("{address}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let verified = b"correct content";
    let digest = blake3::hash(verified).to_hex().to_string();
    let url = format!("{address}/v1/uploads/{digest}/{}", verified.len());
    let corrupted = probe
        .put(format!("{url}/0"))
        .bearer_auth(&token)
        .body(vec![b'x'; verified.len()])
        .send()
        .await
        .unwrap();
    assert_eq!(
        corrupted.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    let progress: serde_json::Value = probe
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(progress["offset"], 0);
    let completed: serde_json::Value = probe
        .put(format!("{url}/0"))
        .bearer_auth(&token)
        .body(verified.to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["complete"], true);
    let actual = probe
        .get(format!("{address}/v1/objects/{digest}/{}", verified.len()))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(actual.as_ref(), verified);

    let interrupted_bytes = b"unfinished transfer across restart";
    let interrupted_digest = blake3::hash(interrupted_bytes).to_hex().to_string();
    let upload_url = format!(
        "{address}/v1/uploads/{interrupted_digest}/{}",
        interrupted_bytes.len()
    );
    let partial = probe
        .put(format!("{upload_url}/0"))
        .bearer_auth(&token)
        .body(interrupted_bytes[..10].to_vec())
        .send()
        .await
        .unwrap();
    assert!(partial.status().is_success());
    let progress: serde_json::Value = partial.json().await.unwrap();
    assert_eq!(progress["offset"], 10);
    let stale_chunk = probe
        .put(format!("{upload_url}/0"))
        .bearer_auth(&token)
        .body(interrupted_bytes[..10].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stale_chunk.status(), reqwest::StatusCode::CONFLICT);
    let progress: serde_json::Value = probe
        .get(&upload_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(progress["offset"], 10);
    drop(_server);
    let _restarted = Server(command.env("GAMESYNC_KEEP_COUNT", "50").spawn().unwrap());
    for _ in 0..50 {
        if probe.get(format!("{address}/healthz")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let status = session.status().await.unwrap();
    assert_eq!((status.keep_count, status.keep_days), (15, 45));
    assert_eq!(session.head(game).await.unwrap(), Some(restored));
    let revoked_response = probe
        .put(format!("{transfer_url}/5"))
        .bearer_auth(&second_token)
        .body(transfer[5..].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let restored_manifest = session.revision(game, restored).await.unwrap().manifest;
    let previous_manifest = session.revision(game, resolved).await.unwrap().manifest;
    assert_eq!(
        restored_manifest
            .entries()
            .iter()
            .find(|file| file.root == entry.root)
            .unwrap()
            .digest,
        blake3::hash(b"save AA").to_hex().as_str()
    );
    assert_eq!(
        previous_manifest
            .entries()
            .iter()
            .find(|file| file.root == entry.root)
            .unwrap()
            .digest,
        blake3::hash(b"save BB").to_hex().as_str()
    );
    let progress: serde_json::Value = probe
        .get(&upload_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(progress["offset"], 10);
    let completed: serde_json::Value = probe
        .put(format!("{upload_url}/10"))
        .bearer_auth(&token)
        .body(interrupted_bytes[10..].to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["complete"], true);
    let downloaded = probe
        .get(format!(
            "{address}/v1/objects/{interrupted_digest}/{}",
            interrupted_bytes.len()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(downloaded.as_ref(), interrupted_bytes);
}

#[test]
fn remote_cleartext_addresses_are_rejected() {
    assert!(Api::new("http://192.0.2.1:8787").is_err());
    assert!(Api::new("https://server.example.test").is_ok());
}

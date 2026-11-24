#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity;
mod background;
mod config;
#[cfg(all(debug_assertions, feature = "desktop-smoke"))]
mod smoke;
use config::{Config, Game, Result, SaveRoot, Server};
use gamesync_protocol::{Api, Credentials, Session, TransferOptions, TransferProgress};
use gamesync_sync::{Outcome, Workflow};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use sysinfo::{ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct App {
    data: PathBuf,
    config: Mutex<Config>,
    operation: tokio::sync::Mutex<()>,
    quitting: AtomicBool,
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl App {
    fn config(&self) -> Result<std::sync::MutexGuard<'_, Config>> {
        self.config.lock().map_err(failure)
    }

    fn session(&self) -> Result<Session> {
        let server = self
            .config()?
            .server
            .clone()
            .ok_or("Appairez d’abord cet appareil.")?;
        let entry = credential(server.device)?;
        let token = entry.get_password().map_err(failure)?;
        Ok(Api::new(&server.url)
            .map_err(failure)?
            .authenticate(Credentials {
                device: server.device,
                token,
            }))
    }

    fn game(&self, id: Uuid) -> Result<Game> {
        self.config()?
            .games
            .iter()
            .find(|game| game.id == id)
            .cloned()
            .ok_or("Jeu introuvable.".into())
    }
}

fn credential(device: Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new("io.gamesync.desktop", &device.to_string()).map_err(failure)
}

#[tauri::command]
fn configuration(app: State<'_, App>) -> Result<serde_json::Value> {
    serde_json::to_value(&*app.config()?).map_err(failure)
}

#[tauri::command]
async fn pair(app: State<'_, App>, address: String, code: String, name: String) -> Result<()> {
    let api = Api::new(address.trim()).map_err(failure)?;
    let credentials = api.pair(code.trim(), name.trim()).await.map_err(failure)?;
    credential(credentials.device)?
        .set_password(&credentials.token)
        .map_err(failure)?;
    let mut configuration = app.config()?;
    let old = configuration.server.replace(Server {
        url: address.trim().into(),
        device: credentials.device,
    });
    if let Err(error) = config::save(&app.data, &configuration) {
        configuration.server = old;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
async fn discover(
    app: State<'_, App>,
    catalog: Option<PathBuf>,
) -> Result<Vec<gamesync_discovery::Installation>> {
    let catalog = catalog.or_else(|| {
        let path = app.data.join("ludusavi.yaml");
        path.is_file().then_some(path)
    });
    tauri::async_runtime::spawn_blocking(move || gamesync_discovery::discover(catalog.as_deref()))
        .await
        .map_err(failure)?
}

#[derive(Deserialize)]
struct RootInput {
    id: Option<Uuid>,
    label: String,
    directory: PathBuf,
}

#[tauri::command]
fn add_game(
    app: State<'_, App>,
    title: String,
    executable: PathBuf,
    game_id: Option<Uuid>,
    roots: Vec<RootInput>,
) -> Result<()> {
    if roots
        .iter()
        .any(|root| root.id.is_some() != game_id.is_some())
    {
        return Err("Les identifiants de rattachement doivent être renseignés ensemble.".into());
    }
    let mut config = app.config()?;
    let id = game_id.unwrap_or_else(Uuid::new_v4);
    if config.games.iter().any(|game| game.id == id) {
        return Err("Ce jeu est déjà configuré.".into());
    }
    let game = build_game(&app.data, &config, id, title, executable, roots, true)?;
    config.games.push(game);
    if let Err(error) = config::save(&app.data, &config) {
        config.games.pop();
        return Err(error);
    }
    Ok(())
}

fn build_game(
    data: &Path,
    config: &Config,
    id: Uuid,
    title: String,
    executable: PathBuf,
    roots: Vec<RootInput>,
    tracking: bool,
) -> Result<Game> {
    if !(1..=16).contains(&roots.len()) || !executable.is_absolute() || !executable.is_file() {
        return Err(
            "Indiquez un exécutable existant et entre un et seize dossiers de sauvegarde.".into(),
        );
    }
    let mut paths: Vec<SaveRoot> = Vec::with_capacity(roots.len());
    let overlaps = |left: &Path, right: &Path| left.starts_with(right) || right.starts_with(left);
    for root in roots {
        if !root.directory.is_absolute() || !root.directory.is_dir() {
            return Err("Chaque dossier de sauvegarde doit exister.".into());
        }
        let directory = root.directory.canonicalize().map_err(failure)?;
        if overlaps(&directory, data)
            || paths
                .iter()
                .any(|root| overlaps(&directory, &root.directory))
            || config
                .games
                .iter()
                .filter(|game| game.id != id)
                .flat_map(Game::paths)
                .any(|(_, path)| overlaps(&directory, path))
        {
            return Err("Les dossiers de sauvegarde doivent être distincts et hors des données de GameSync.".into());
        }
        paths.push(SaveRoot {
            id: root.id.unwrap_or_else(Uuid::new_v4),
            label: root.label.trim().into(),
            directory,
        });
    }
    let mut roots = paths.into_iter();
    let first = roots.next().expect("validated non-empty root list");
    let game = Game {
        id,
        root: first.id,
        root_label: first.label,
        title: title.trim().into(),
        directory: first.directory,
        executable: executable.canonicalize().map_err(failure)?,
        additional_roots: roots.collect(),
        tracking,
    };
    if !game.registration().is_valid() {
        return Err("Indiquez un nom de jeu et un libellé valide pour chaque dossier.".into());
    }
    Ok(game)
}

#[tauri::command]
fn update_game(
    app: State<'_, App>,
    id: Uuid,
    executable: PathBuf,
    roots: Vec<RootInput>,
) -> Result<()> {
    let _operation = app
        .operation
        .try_lock()
        .map_err(|_| "Une synchronisation est en cours.")?;
    let mut settings = app.config()?;
    let index = settings
        .games
        .iter()
        .position(|game| game.id == id)
        .ok_or("Jeu introuvable.")?;
    let previous = settings.games[index].clone();
    if previous.executable.is_file() && !idle(&previous.executable) {
        return Err("Fermez le jeu avant de modifier ses chemins.".into());
    }
    let registration = previous.registration();
    let mut by_id = std::collections::HashMap::new();
    for root in roots {
        let id = root.id.ok_or("L’identifiant du dossier est requis.")?;
        if by_id.insert(id, root.directory).is_some() {
            return Err("Un dossier apparaît plusieurs fois.".into());
        }
    }
    if by_id.len() != registration.roots.len() {
        return Err("Les dossiers doivent correspondre à ceux du jeu enregistré.".into());
    }
    let roots = registration
        .roots
        .into_iter()
        .map(|root| {
            let directory = by_id
                .remove(&root.id)
                .ok_or("Un dossier enregistré est manquant.")?;
            Ok(RootInput {
                id: Some(root.id),
                label: root.label,
                directory,
            })
        })
        .collect::<std::result::Result<Vec<_>, &str>>()?;
    let game = build_game(
        &app.data,
        &settings,
        id,
        previous.title.clone(),
        executable,
        roots,
        previous.tracking,
    )?;
    settings.games[index] = game;
    if let Err(error) = config::save(&app.data, &settings) {
        settings.games[index] = previous;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
fn set_tracking(app: State<'_, App>, id: Uuid, tracking: bool) -> Result<()> {
    let _operation = app
        .operation
        .try_lock()
        .map_err(|_| "Une synchronisation est en cours.")?;
    let mut settings = app.config()?;
    let index = settings
        .games
        .iter()
        .position(|game| game.id == id)
        .ok_or("Jeu introuvable.")?;
    let previous = settings.games[index].tracking;
    settings.games[index].tracking = tracking;
    if let Err(error) = config::save(&app.data, &settings) {
        settings.games[index].tracking = previous;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
async fn server_games(app: State<'_, App>) -> Result<Vec<gamesync_protocol::GameRegistration>> {
    app.session()?.games().await.map_err(failure)
}

fn idle(executable: &Path) -> bool {
    let Some(name) = executable.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !executable.is_file() {
        return false;
    }
    let mut system = System::new();
    if system.refresh_processes(ProcessesToUpdate::All, true) == 0 {
        return false;
    }
    !system.processes().values().any(|process| {
        process.name().to_string_lossy().eq_ignore_ascii_case(name)
            || process
                .exe()
                .and_then(Path::file_name)
                .is_some_and(|other| other.to_string_lossy().eq_ignore_ascii_case(name))
    })
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SyncStatus {
    Unchanged,
    Synchronized { revision: i64 },
    Conflict { variant: i64 },
    Deferred,
}

#[derive(Clone, Serialize)]
struct TransferEvent {
    game: Uuid,
    title: String,
    progress: Option<TransferProgress>,
}

struct TransferBroadcast {
    handle: AppHandle,
    game: Uuid,
    title: String,
}

impl TransferBroadcast {
    fn send(&self, progress: Option<TransferProgress>) {
        let _ = self.handle.emit(
            "gamesync-transfer",
            TransferEvent {
                game: self.game,
                title: self.title.clone(),
                progress,
            },
        );
    }
}

impl Drop for TransferBroadcast {
    fn drop(&mut self) {
        self.send(None);
    }
}

#[tauri::command]
async fn synchronize(
    app: State<'_, App>,
    handle: AppHandle,
    id: Uuid,
    revision: Option<i64>,
    expected_head: Option<i64>,
) -> Result<SyncStatus> {
    let game = app.game(id)?;
    let _guard = app
        .operation
        .try_lock()
        .map_err(|_| "Une synchronisation est en cours.")?;
    if app.quitting.load(Ordering::Relaxed) {
        return Err("GameSync est en cours d’arrêt.".into());
    }
    let result =
        synchronize_game(&app, &handle, game.clone(), revision, expected_head, false).await;
    record_manual_sync(&handle, &game, &result);
    result
}

async fn synchronize_game(
    app: &App,
    handle: &AppHandle,
    game: Game,
    revision: Option<i64>,
    expected_head: Option<i64>,
    automatic: bool,
) -> Result<SyncStatus> {
    let game = app.game(game.id)?;
    if !game.tracking {
        return Err("Le suivi de ce jeu est en pause.".into());
    }
    let id = game.id;
    let broadcast = std::sync::Arc::new(TransferBroadcast {
        handle: handle.clone(),
        game: id,
        title: game.title.clone(),
    });
    let bandwidth = std::num::NonZeroU64::new(u64::from(app.config()?.bandwidth_kib) * 1024);
    let session = app.session()?.with_transfer_options(TransferOptions {
        bytes_per_second: bandwidth,
        observer: Some(std::sync::Arc::new(move |progress| {
            broadcast.send(Some(progress))
        })),
    });
    session
        .register_game(&game.registration())
        .await
        .map_err(failure)?;
    let roots = game
        .paths()
        .map(|(id, path)| (id, path.to_path_buf()))
        .collect::<Vec<_>>();
    let directory = app
        .data
        .join("games")
        .join(session.device().to_string())
        .join(id.to_string());
    let workflow = Workflow {
        game: id,
        roots: &roots,
        state_dir: &directory,
        session: &session,
    };
    let check = || idle(&game.executable);
    let result = match (revision, expected_head) {
        (None, None) if automatic => workflow.run_automatic(check).await,
        (None, None) => workflow.run(check).await,
        (Some(revision), Some(head)) => workflow.choose_revision(revision, head, check).await,
        _ => return Err("La révision courante est requise pour restaurer.".into()),
    };
    Ok(match result.map_err(failure)? {
        Outcome::Unchanged => SyncStatus::Unchanged,
        Outcome::Synchronized(revision) => SyncStatus::Synchronized { revision },
        Outcome::Conflict { variant } => SyncStatus::Conflict { variant },
        Outcome::Deferred => SyncStatus::Deferred,
    })
}

impl SyncStatus {
    fn activity_message(&self, locale: config::Locale) -> (&'static str, String) {
        use config::Locale;
        match self {
            Self::Unchanged => (
                "unchanged",
                locale
                    .text("Sauvegardes à jour", "Saves are up to date")
                    .into(),
            ),
            Self::Synchronized { revision } => (
                "synchronized",
                match locale {
                    Locale::Fr => format!("Révision #{revision} synchronisée"),
                    Locale::En => format!("Revision #{revision} synchronized"),
                },
            ),
            Self::Conflict { variant } => (
                "conflict",
                match locale {
                    Locale::Fr => format!("Variante #{variant} en conflit"),
                    Locale::En => format!("Conflict variant #{variant}"),
                },
            ),
            Self::Deferred => (
                "deferred",
                locale
                    .text("Synchronisation reportée", "Synchronization deferred")
                    .into(),
            ),
        }
    }
}

fn record_manual_sync(app: &AppHandle, game: &Game, result: &Result<SyncStatus>) {
    let locale = app
        .state::<App>()
        .config()
        .map(|settings| settings.locale)
        .unwrap_or_default();
    let (status, message) = match result {
        Ok(result) => result.activity_message(locale),
        Err(error) => ("error", error.clone()),
    };
    activity::record(
        app,
        Some(game.id),
        Some(status),
        format!("{} · {message}", game.title),
        false,
    );
}

#[tauri::command]
fn recent_activity(app: State<'_, App>) -> Result<Vec<activity::Event>> {
    activity::list(&app.data).map_err(failure)
}

#[tauri::command]
async fn synchronize_and_launch(
    app: State<'_, App>,
    handle: tauri::AppHandle,
    id: Uuid,
) -> Result<SyncStatus> {
    let _operation = app
        .operation
        .try_lock()
        .map_err(|_| "Une synchronisation est en cours.")?;
    if app.quitting.load(Ordering::Relaxed) {
        return Err("GameSync est en cours d’arrêt.".into());
    }
    let game = app.game(id)?;
    let result = synchronize_game(&app, &handle, game.clone(), None, None, false).await;
    record_manual_sync(&handle, &game, &result);
    let status = result?;
    if matches!(
        status,
        SyncStatus::Unchanged | SyncStatus::Synchronized { .. }
    ) {
        if !idle(&game.executable) || app.quitting.load(Ordering::Relaxed) {
            return Err("Le jeu est déjà actif ou GameSync est en cours d’arrêt.".into());
        }
        let parent = game
            .executable
            .parent()
            .ok_or("Dossier de lancement introuvable.")?;
        let mut child = std::process::Command::new(&game.executable)
            .current_dir(parent)
            .spawn()
            .map_err(failure)?;
        std::thread::spawn(move || {
            if child.wait().is_ok() {
                background::game_exited(&handle, id);
            }
        });
    }
    Ok(status)
}

#[tauri::command]
async fn history(app: State<'_, App>, id: Uuid, before: Option<i64>) -> Result<serde_json::Value> {
    app.game(id)?;
    let session = app.session()?;
    let head = session.head(id).await.map_err(failure)?;
    let revisions = session.history(id, before).await.map_err(failure)?;
    let items = revisions
        .into_iter()
        .map(|revision| {
            serde_json::json!({
                "id": revision.id, "machine": revision.machine, "created_at": revision.created_at,
                "published": revision.published, "bytes": revision.bytes, "resolved_by": revision.resolved_by,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "head": head, "items": items }))
}

#[tauri::command]
async fn server_status(app: State<'_, App>) -> Result<gamesync_protocol::ServerStatus> {
    app.session()?.status().await.map_err(failure)
}

#[tauri::command]
async fn set_retention(app: State<'_, App>, keep_count: u32, keep_days: u32) -> Result<()> {
    app.session()?
        .set_retention(keep_count, keep_days)
        .await
        .map_err(failure)
}

#[tauri::command]
async fn devices(app: State<'_, App>) -> Result<gamesync_protocol::DeviceList> {
    app.session()?.devices().await.map_err(failure)
}

#[tauri::command]
async fn invite_device(app: State<'_, App>) -> Result<String> {
    app.session()?.invite_device().await.map_err(failure)
}

#[tauri::command]
async fn revoke_device(app: State<'_, App>, id: Uuid) -> Result<()> {
    app.session()?.revoke_device(id).await.map_err(failure)
}

#[tauri::command]
fn automation(
    app: State<'_, App>,
    automatic: bool,
    interval_seconds: u64,
    bandwidth_kib: u32,
    notifications: bool,
) -> Result<()> {
    if !(30..=3600).contains(&interval_seconds) {
        return Err("Choisissez une fréquence entre 30 et 3600 secondes.".into());
    }
    if bandwidth_kib != 0 && !(16..=1_048_576).contains(&bandwidth_kib) {
        return Err(
            "Choisissez 0 pour un débit illimité, ou une limite entre 16 et 1 048 576 Kio/s."
                .into(),
        );
    }
    let mut settings = app.config()?;
    let previous = (
        settings.automatic,
        settings.interval_seconds,
        settings.bandwidth_kib,
        settings.notifications,
    );
    settings.automatic = automatic;
    settings.interval_seconds = interval_seconds;
    settings.bandwidth_kib = bandwidth_kib;
    settings.notifications = notifications;
    if let Err(error) = config::save(&app.data, &settings) {
        (
            settings.automatic,
            settings.interval_seconds,
            settings.bandwidth_kib,
            settings.notifications,
        ) = previous;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
fn set_theme(app: State<'_, App>, theme: config::Theme) -> Result<()> {
    let mut settings = app.config()?;
    let previous = settings.theme;
    settings.theme = theme;
    if let Err(error) = config::save(&app.data, &settings) {
        settings.theme = previous;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
fn set_locale(app: State<'_, App>, locale: config::Locale) -> Result<()> {
    let mut settings = app.config()?;
    let previous = settings.locale;
    settings.locale = locale;
    if let Err(error) = config::save(&app.data, &settings) {
        settings.locale = previous;
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    background::quit_when_idle(&app);
}

#[tauri::command]
fn autostart(app: tauri::AppHandle, enabled: Option<bool>) -> Result<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    match enabled {
        Some(true) => manager.enable().map_err(failure)?,
        Some(false) => manager.disable().map_err(failure)?,
        None => {}
    }
    manager.is_enabled().map_err(failure)
}

fn main() {
    let builder = tauri::Builder::default();
    #[cfg(all(debug_assertions, feature = "desktop-smoke"))]
    let builder = builder.on_page_load(smoke::page_loaded);
    builder
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--background"])
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let data = data.canonicalize()?;
            let config = config::load(&data).map_err(std::io::Error::other)?;
            app.manage(App {
                data,
                config: Mutex::new(config),
                operation: tokio::sync::Mutex::new(()),
                quitting: AtomicBool::new(false),
            });
            background::setup(app.handle());
            if std::env::args().any(|argument| argument == "--background")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.minimize();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                #[cfg(target_os = "linux")]
                let visible_tray = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| {
                    desktop.split(':').any(|name| {
                        matches!(
                            name.to_ascii_lowercase().as_str(),
                            "kde" | "xfce" | "lxqt" | "cinnamon"
                        )
                    })
                });
                #[cfg(not(target_os = "linux"))]
                let visible_tray = true;
                if visible_tray && window.app_handle().tray_by_id("main").is_some() {
                    let _ = window.hide();
                } else {
                    let _ = window.minimize();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(all(debug_assertions, feature = "desktop-smoke"))]
            smoke::smoke_result,
            configuration,
            pair,
            add_game,
            update_game,
            set_tracking,
            synchronize_and_launch,
            server_games,
            discover,
            synchronize,
            history,
            recent_activity,
            devices,
            server_status,
            set_retention,
            invite_device,
            revoke_device,
            automation,
            set_theme,
            set_locale,
            autostart,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("GameSync desktop failed");
}

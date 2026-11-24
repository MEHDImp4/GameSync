use crate::{App, SyncStatus, activity, config::Game, synchronize_game};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{
    AppHandle, Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tokio::{
    sync::mpsc,
    time::{self, Instant},
};
use uuid::Uuid;

const DEBOUNCE: Duration = Duration::from_secs(2);
const RUNNING_RETRY: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Schedule {
    due: Option<Instant>,
    failures: u32,
    status: Option<&'static str>,
}

impl Schedule {
    fn schedule_missed_events(&mut self, missed: bool, now: Instant) {
        if missed && self.failures == 0 {
            let next = now + DEBOUNCE;
            self.due = Some(self.due.map_or(next, |scheduled| scheduled.min(next)));
        }
    }
}

async fn acquire_operation(
    operation: &tokio::sync::Mutex<()>,
    manual: bool,
) -> Option<tokio::sync::MutexGuard<'_, ()>> {
    if manual {
        Some(operation.lock().await)
    } else {
        operation.try_lock().ok()
    }
}

pub fn setup(app: &AppHandle) {
    let (trigger, receiver) = mpsc::channel(256);
    app.manage(GameExit(trigger.clone()));
    let background = app.clone();
    tauri::async_runtime::spawn(async move { run(background, receiver).await });
    if let Err(error) = tray(app, trigger) {
        eprintln!("GameSync tray unavailable: {error}");
    }
}

struct GameExit(mpsc::Sender<Trigger>);

pub fn game_exited(app: &AppHandle, game: Uuid) {
    let _ = app
        .state::<GameExit>()
        .0
        .try_send(Trigger::GameExited(game));
}

enum Trigger {
    Files(Vec<PathBuf>, bool),
    GameExited(Uuid),
    All,
    WatchError(String),
}

fn enqueue_watch_event(events: &mpsc::Sender<Trigger>, overflow: &AtomicBool, trigger: Trigger) {
    if events.try_send(trigger).is_err() {
        overflow.store(true, Ordering::Release);
    }
}

fn watch_trigger(event: notify::Result<notify::Event>, overflow: &AtomicBool) -> Option<Trigger> {
    match event {
        Ok(event) => {
            if event.need_rescan() {
                overflow.store(true, Ordering::Release);
            }
            if event.kind.is_access() && !event.need_rescan() {
                return None;
            }
            let may_replace = event.kind.is_remove()
                || matches!(
                    event.kind,
                    notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                );
            Some(Trigger::Files(event.paths, may_replace))
        }
        Err(error) => Some(Trigger::WatchError(error.to_string())),
    }
}

async fn run(app: AppHandle, mut triggers: mpsc::Receiver<Trigger>) {
    let state = app.state::<App>();
    let initial_locale = state
        .config()
        .map(|settings| settings.locale)
        .unwrap_or_default();
    let (events, mut watcher_events) = mpsc::channel(256);
    let overflow = Arc::new(AtomicBool::new(false));
    let watcher_overflow = overflow.clone();
    let parent_events = events.clone();
    let parent_overflow = overflow.clone();
    let mut parent_watcher = notify::recommended_watcher(move |event| {
        if let Some(trigger) = watch_trigger(event, &parent_overflow) {
            enqueue_watch_event(&parent_events, &parent_overflow, trigger);
        }
    })
    .ok();
    let mut watched_parents = HashSet::new();
    let mut watcher =
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Some(trigger) = watch_trigger(event, &watcher_overflow) else {
                return;
            };
            enqueue_watch_event(&events, &watcher_overflow, trigger);
        }) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                emit(
                    &app,
                    None,
                    format!(
                        "{} {error}",
                        initial_locale.text(
                            "Surveillance indisponible. Vérifiez les réglages de synchronisation.",
                            "File watching unavailable. Check synchronization settings.",
                        )
                    ),
                );
                None
            }
        };
    let mut watched: HashSet<PathBuf> = HashSet::new();
    let mut watch_failures = HashSet::new();
    let mut schedule = HashMap::<Uuid, Schedule>::new();
    let mut last_tooltip = String::new();
    let mut timer = time::interval(DEBOUNCE);
    timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    loop {
        let trigger = tokio::select! {
            _ = timer.tick() => None,
            event = watcher_events.recv(), if watcher.is_some() => event,
            event = triggers.recv(), if !triggers.is_closed() => event,
        };
        if state.quitting.load(Ordering::Relaxed) {
            return;
        }
        let (paired, automatic, period, games, locale) = match state.config() {
            Ok(config) => (
                config.server.is_some(),
                config.automatic,
                Duration::from_secs(config.interval_seconds),
                config.games.clone(),
                config.locale,
            ),
            Err(error) => {
                emit(&app, None, error);
                return;
            }
        };
        let desired: HashSet<_> = games
            .iter()
            .filter(|game| automatic && paired && game.tracking)
            .flat_map(Game::paths)
            .map(|(_, path)| path.to_path_buf())
            .collect();
        let queue_overflowed = overflow.swap(false, Ordering::AcqRel);
        let missed_events = queue_overflowed || matches!(&trigger, Some(Trigger::WatchError(_)));
        if let Some(parent_watcher) = &mut parent_watcher {
            update_parent_watches(parent_watcher, &mut watched_parents, &desired);
        }
        if queue_overflowed {
            emit(
                &app,
                None,
                locale.text(
                    "Des événements de surveillance ont pu être perdus. Vérifiez l’état de synchronisation des jeux.",
                    "Some file events may have been missed. Check the synchronization status of your games.",
                ).into(),
            );
        }
        if let Some(watcher) = &mut watcher {
            if missed_events {
                reset_watches(watcher, &mut watched);
            }
            invalidate_replaced_watches(watcher, &mut watched, trigger.as_ref());
            update_watches(
                watcher,
                &mut watched,
                &desired,
                &mut watch_failures,
                |path, error| {
                    emit(
                        &app,
                        None,
                        format!(
                            "{} {}: {error}",
                            locale.text("Surveillance impossible pour", "Cannot watch"),
                            path.display()
                        ),
                    )
                },
            );
        }
        schedule.retain(|id, _| {
            paired && automatic && games.iter().any(|game| game.id == *id && game.tracking)
        });
        if let Some(Trigger::WatchError(error)) = &trigger {
            emit(
                &app,
                None,
                format!(
                    "{} {error}",
                    locale.text(
                        "Erreur de surveillance. Vérifiez l’état de synchronisation des jeux.",
                        "File watching error. Check the synchronization status of your games.",
                    )
                ),
            );
        }
        let now = Instant::now();
        for game in games.iter().filter(|game| paired && game.tracking) {
            let manual = matches!(&trigger, Some(Trigger::All));
            if !automatic && !manual {
                continue;
            }
            let task = schedule.entry(game.id).or_insert(Schedule {
                due: Some(now),
                ..Schedule::default()
            });
            if let Some(Trigger::Files(paths, _)) = &trigger
                && task.failures == 0
                && paths
                    .iter()
                    .any(|path| game.paths().any(|(_, root)| path.starts_with(root)))
            {
                task.schedule_missed_events(true, now);
            }
            task.schedule_missed_events(missed_events && automatic, now);
            if matches!(&trigger, Some(Trigger::GameExited(id)) if *id == game.id) && automatic {
                task.schedule_missed_events(true, now);
            }
            if !manual && !task.due.is_some_and(|due| due <= now) {
                continue;
            }
            let (_guard, result) = {
                let Some(guard) = acquire_operation(&state.operation, manual).await else {
                    continue;
                };
                if state.quitting.load(Ordering::Relaxed) {
                    return;
                }
                let result =
                    synchronize_game(&state, &app, game.clone(), None, None, !manual).await;
                (guard, result)
            };
            let now = Instant::now();
            let (status, delay, message) = match result {
                Ok(result) => {
                    let delay = if matches!(result, SyncStatus::Deferred) {
                        RUNNING_RETRY
                    } else {
                        period
                    };
                    let (status, message) = result.activity_message(locale);
                    (status, delay, message)
                }
                Err(error) => {
                    task.failures = task.failures.saturating_add(1);
                    let delay = Duration::from_secs((5 << task.failures.min(8)).min(900) as u64);
                    (
                        "error",
                        delay,
                        format!(
                            "{error} · {} {} s",
                            locale.text("Reprise dans", "Retry in"),
                            delay.as_secs()
                        ),
                    )
                }
            };
            if manual
                || matches!(status, "synchronized" | "error")
                || (task.status != Some(status) && (status != "unchanged" || task.status.is_some()))
            {
                activity::record(
                    &app,
                    Some(game.id),
                    Some(status),
                    format!("{} · {message}", game.title),
                    matches!(status, "conflict" | "error") && task.status != Some(status),
                );
            }
            task.status = Some(status);
            task.due = Some(now + delay);
            if status != "error" {
                task.failures = 0;
            }
        }
        let status = if !paired {
            locale.text("Appairage requis", "Pairing required")
        } else if (automatic && (watcher.is_none() || !watch_failures.is_empty()))
            || schedule
                .values()
                .any(|task| matches!(task.status, Some("error" | "conflict")))
        {
            locale.text("Attention requise", "Attention required")
        } else if !automatic {
            locale.text(
                "Synchronisation automatique désactivée",
                "Automatic sync disabled",
            )
        } else {
            locale.text(
                "Synchronisation automatique activée",
                "Automatic sync enabled",
            )
        };
        let tooltip = format!("GameSync · {status}");
        if tooltip != last_tooltip
            && let Some(tray) = app.tray_by_id("main")
        {
            if tray.set_tooltip(Some(&tooltip)).is_ok() {
                last_tooltip = tooltip;
            }
        }
    }
}

fn reset_watches(watcher: &mut RecommendedWatcher, watched: &mut HashSet<PathBuf>) {
    for path in watched.drain() {
        let _ = watcher.unwatch(&path);
    }
}

fn update_parent_watches(
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    desired: &HashSet<PathBuf>,
) {
    let parents: HashSet<_> = desired
        .iter()
        .filter_map(|path| path.parent())
        .filter(|path| path.is_dir())
        .map(|path| path.to_path_buf())
        .collect();
    for path in watched.difference(&parents) {
        let _ = watcher.unwatch(path);
    }
    watched.retain(|path| parents.contains(path));
    for path in parents.difference(watched).cloned().collect::<Vec<_>>() {
        if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
            watched.insert(path);
        }
    }
}

fn invalidate_replaced_watches(
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    trigger: Option<&Trigger>,
) {
    if let Some(Trigger::Files(paths, true)) = trigger {
        for path in paths {
            if watched.remove(path) {
                let _ = watcher.unwatch(path);
            }
        }
    }
}

fn update_watches(
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    desired: &HashSet<PathBuf>,
    failures: &mut HashSet<PathBuf>,
    mut report: impl FnMut(&std::path::Path, &notify::Error),
) {
    failures.retain(|path| desired.contains(path));
    for path in watched
        .iter()
        .filter(|path| !desired.contains(*path) || !path.is_dir())
    {
        let _ = watcher.unwatch(path);
    }
    watched.retain(|path| desired.contains(path) && path.is_dir());
    for path in desired {
        if watched.contains(path) {
            continue;
        }
        match watcher.watch(path, RecursiveMode::Recursive) {
            Ok(()) => {
                watched.insert(path.clone());
                failures.remove(path);
            }
            Err(error) if failures.insert(path.clone()) => report(path, &error),
            Err(_) => {}
        }
    }
}

fn emit(app: &AppHandle, game: Option<Uuid>, message: String) {
    activity::record(app, game, Some("error"), message, false);
}

fn tray(app: &AppHandle, trigger: mpsc::Sender<Trigger>) -> tauri::Result<()> {
    let locale = app
        .state::<App>()
        .config()
        .map(|settings| settings.locale)
        .unwrap_or_default();
    let open = MenuItem::with_id(
        app,
        "open",
        locale.text("Ouvrir GameSync", "Open GameSync"),
        true,
        None::<&str>,
    )?;
    let sync = MenuItem::with_id(
        app,
        "sync",
        locale.text("Synchroniser", "Sync"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        locale.text("Quitter", "Quit"),
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&open, &sync, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("GameSync");
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(move |app, event| match event.id.as_ref() {
        "open" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        "sync" => {
            let _ = trigger.try_send(Trigger::All);
        }
        "quit" => quit_when_idle(app),
        _ => {}
    })
    .build(app)?;
    Ok(())
}

pub fn quit_when_idle(app: &AppHandle) {
    let app = app.clone();
    let state = app.state::<App>();
    if state.quitting.swap(true, Ordering::Relaxed) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let state = app.state::<App>();
        let _guard = state.operation.lock().await;
        app.exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_sync_waits_until_the_current_operation_finishes() {
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let running = operation.lock().await;
        assert!(acquire_operation(&operation, false).await.is_none());
        let requested = operation.clone();
        let manual = tokio::spawn(async move {
            let _guard = acquire_operation(&requested, true).await.unwrap();
        });
        tokio::task::yield_now().await;
        assert!(!manual.is_finished());
        drop(running);
        tokio::time::timeout(Duration::from_secs(2), manual)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn rescan_notice_is_kept_when_event_queue_is_full() {
        let (sender, mut receiver) = mpsc::channel(1);
        let overflow = AtomicBool::new(false);
        enqueue_watch_event(&sender, &overflow, Trigger::Files(vec![], false));
        let event =
            notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan);
        let trigger = watch_trigger(Ok(event), &overflow).unwrap();
        enqueue_watch_event(&sender, &overflow, trigger);
        assert!(receiver.try_recv().is_ok());
        let missed = overflow.swap(false, Ordering::AcqRel);
        let now = Instant::now();
        let mut schedule = Schedule::default();
        schedule.schedule_missed_events(missed, now);
        assert_eq!(schedule.due, Some(now + DEBOUNCE));
    }

    #[test]
    fn full_event_queue_schedules_rescan_without_overriding_backoff() {
        let (sender, mut receiver) = mpsc::channel(1);
        let overflow = AtomicBool::new(false);
        enqueue_watch_event(&sender, &overflow, Trigger::Files(vec![], false));
        assert!(!overflow.load(Ordering::Acquire));
        enqueue_watch_event(&sender, &overflow, Trigger::Files(vec![], false));
        assert!(receiver.try_recv().is_ok());

        let missed = overflow.swap(false, Ordering::AcqRel);
        assert!(missed);
        let now = Instant::now();
        let mut normal = Schedule::default();
        normal.schedule_missed_events(missed, now);
        assert_eq!(normal.due, Some(now + DEBOUNCE));
        normal.schedule_missed_events(true, now + Duration::from_secs(1));
        assert_eq!(normal.due, Some(now + DEBOUNCE));

        let retry = now + Duration::from_secs(90);
        let mut failed = Schedule {
            due: Some(retry),
            failures: 2,
            status: Some("error"),
        };
        failed.schedule_missed_events(missed, now);
        assert_eq!(failed.due, Some(retry));
        assert!(!overflow.load(Ordering::Acquire));
    }

    #[test]
    fn overflow_reinstates_watch_on_replaced_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("saves");
        std::fs::create_dir(&root).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let desired = HashSet::from([root.clone()]);
        let mut watched = HashSet::new();
        let mut failures = HashSet::new();
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        std::fs::rename(&root, workspace.path().join("old")).unwrap();
        std::fs::create_dir(&root).unwrap();

        let (events, _pending) = mpsc::channel(1);
        let overflow = AtomicBool::new(false);
        enqueue_watch_event(&events, &overflow, Trigger::Files(vec![], false));
        enqueue_watch_event(&events, &overflow, Trigger::Files(vec![root.clone()], true));
        assert!(overflow.swap(false, Ordering::AcqRel));
        reset_watches(&mut watcher, &mut watched);
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        while receiver.try_recv().is_ok() {}
        let save = root.join("progress.sav");
        std::fs::write(&save, b"progress").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
                .unwrap();
            if event.paths.contains(&save) {
                break;
            }
        }
    }

    #[test]
    fn watcher_follows_rapidly_replaced_root() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("saves");
        std::fs::create_dir(&root).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let parent_sender = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let mut parent_watcher = notify::recommended_watcher(move |event| {
            let _ = parent_sender.send(event);
        })
        .unwrap();
        let desired = HashSet::from([root.clone()]);
        let mut watched_parents = HashSet::new();
        update_parent_watches(&mut parent_watcher, &mut watched_parents, &desired);
        let mut watched = HashSet::new();
        let mut failures = HashSet::new();
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );

        std::fs::rename(&root, workspace.path().join("old")).unwrap();
        std::fs::create_dir(&root).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
                .unwrap();
            if event.paths.contains(&root)
                && matches!(
                    event.kind,
                    notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                )
            {
                invalidate_replaced_watches(
                    &mut watcher,
                    &mut watched,
                    Some(&Trigger::Files(event.paths, true)),
                );
                break;
            }
        }
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        while receiver.try_recv().is_ok() {}
        let save = root.join("progress.sav");
        std::fs::write(&save, b"new progress").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
                .unwrap();
            if event.paths.contains(&save) {
                break;
            }
        }
    }

    #[test]
    fn watcher_recovers_after_root_disappears() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("saves");
        std::fs::create_dir(&root).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let desired = HashSet::from([root.clone()]);
        let mut watched = HashSet::new();
        let mut failures = HashSet::new();
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        assert!(watched.contains(&root));

        std::fs::remove_dir(&root).unwrap();
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        assert!(!watched.contains(&root));
        assert!(failures.contains(&root));

        std::fs::create_dir(&root).unwrap();
        update_watches(
            &mut watcher,
            &mut watched,
            &desired,
            &mut failures,
            |_, _| {},
        );
        assert!(watched.contains(&root));
        assert!(failures.is_empty());
        while receiver.try_recv().is_ok() {}
        let save = root.join("progress.sav");
        std::fs::write(&save, b"progress").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = receiver
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
                .unwrap();
            if event.paths.contains(&save) {
                break;
            }
        }
    }
}

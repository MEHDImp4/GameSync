use crate::App;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct Event {
    pub id: Option<i64>,
    pub time: i64,
    pub game: Option<String>,
    pub status: Option<String>,
    pub message: String,
}

fn database(data: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(data.join("client.sqlite"))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS activity (
             id INTEGER PRIMARY KEY, time INTEGER NOT NULL, game TEXT, status TEXT, message TEXT NOT NULL
         );",
    )?;
    Ok(connection)
}

fn insert(data: &Path, event: &Event) -> rusqlite::Result<i64> {
    let mut db = database(data)?;
    let transaction = db.transaction()?;
    transaction.execute(
        "INSERT INTO activity (time, game, status, message) VALUES (?, ?, ?, ?)",
        params![event.time, event.game, event.status, event.message],
    )?;
    let id = transaction.last_insert_rowid();
    transaction.execute("DELETE FROM activity WHERE id <= ?", [id - 200])?;
    transaction.commit()?;
    Ok(id)
}

pub fn list(data: &Path) -> rusqlite::Result<Vec<Event>> {
    let db = database(data)?;
    let mut statement = db.prepare(
        "SELECT id, time, game, status, message FROM activity ORDER BY id DESC LIMIT 200",
    )?;
    statement
        .query_map([], |row| {
            Ok(Event {
                id: row.get(0)?,
                time: row.get(1)?,
                game: row.get(2)?,
                status: row.get(3)?,
                message: row.get(4)?,
            })
        })?
        .collect()
}

pub fn record(
    app: &AppHandle,
    game: Option<Uuid>,
    status: Option<&str>,
    message: String,
    notify: bool,
) {
    let state = app.state::<App>();
    let mut event = Event {
        id: None,
        time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        game: game.map(|id| id.to_string()),
        status: status.map(str::to_owned),
        message,
    };
    match insert(&state.data, &event) {
        Ok(id) => event.id = Some(id),
        Err(error) => eprintln!("GameSync activity database failed: {error}"),
    }
    let _ = app.emit("gamesync-activity", event);
    if notify
        && let Ok(settings) = state.config()
        && settings.notifications
    {
        let message = match status {
            Some("conflict") => settings.locale.text(
                "Deux versions d’une sauvegarde nécessitent un choix.",
                "Two versions of a save need your attention.",
            ),
            _ => settings.locale.text(
                "Une synchronisation a échoué. Consultez l’onglet Activité.",
                "Synchronization failed. Check the Activity tab.",
            ),
        };
        if let Err(error) = app
            .notification()
            .builder()
            .title("GameSync")
            .body(message)
            .show()
        {
            eprintln!("GameSync notification failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_survives_reopening_and_prunes_the_oldest_rows() {
        let data = tempfile::tempdir().unwrap();
        for time in 1..=201 {
            let event = Event {
                id: None,
                time,
                game: None,
                status: Some("error".into()),
                message: format!("event {time}"),
            };
            insert(data.path(), &event).unwrap();
        }
        let events = list(data.path()).unwrap();
        assert_eq!(events.len(), 200);
        assert_eq!(events.first().unwrap().time, 201);
        assert_eq!(events.last().unwrap().time, 2);
        assert_eq!(events[1].status.as_deref(), Some("error"));
    }
}

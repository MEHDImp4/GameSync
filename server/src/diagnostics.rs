use crate::{Devices, HttpResult, Shared, device_status, disk, object_status, revision_status};
use axum::{
    Json,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize)]
struct RecentError {
    time: u64,
    operation: String,
    status: Option<u16>,
}

#[derive(Default)]
pub struct Diagnostics(Mutex<VecDeque<RecentError>>);

impl Diagnostics {
    pub fn error(&self, operation: String, status: Option<u16>) {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = RecentError {
            time,
            operation,
            status,
        };
        eprintln!(
            "{}",
            serde_json::json!({ "event": "operation_failed", "details": event })
        );
        if let Ok(mut errors) = self.0.lock() {
            if errors.len() == 20 {
                errors.pop_back();
            }
            errors.push_front(event);
        }
    }

    fn recent_errors(&self) -> Vec<RecentError> {
        self.0
            .lock()
            .map(|errors| errors.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Serialize)]
struct Publication {
    revision: i64,
    game: String,
    title: Option<String>,
    machine: String,
    created_at: i64,
}

#[derive(Serialize)]
pub struct Status {
    used_bytes: u64,
    objects: u64,
    synchronized_games: i64,
    keep_count: u32,
    keep_days: u32,
    latest: Vec<Publication>,
    errors: Vec<RecentError>,
}

pub async fn status(State(server): State<Shared>, headers: HeaderMap) -> HttpResult<Json<Status>> {
    server.authorize(&headers).await?;
    disk(move || {
        let (keep_count, keep_days) = Devices::open(&server.data)
            .and_then(|devices| devices.retention(server.retention))
            .map_err(device_status)?;
        let (objects, used_bytes) = server.objects()?.usage().map_err(object_status)?;
        let revisions = server.revisions()?;
        let synchronized_games = revisions.synchronized_count().map_err(revision_status)?;
        let latest = revisions
            .recent_publications()
            .map_err(revision_status)?
            .into_iter()
            .map(|entry| Publication {
                revision: entry.revision,
                game: entry.game,
                title: entry.title,
                machine: entry.machine,
                created_at: entry.created_at,
            })
            .collect();
        Ok(Json(Status {
            used_bytes,
            objects,
            synchronized_games,
            keep_count,
            keep_days,
            latest,
            errors: server.diagnostics.recent_errors(),
        }))
    })
    .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    keep_count: u32,
    keep_days: u32,
}

pub async fn set_retention(
    State(server): State<Shared>,
    headers: HeaderMap,
    Json(policy): Json<Retention>,
) -> HttpResult<StatusCode> {
    let actor = server.authorize(&headers).await?;
    if policy.keep_count == 0 || policy.keep_days == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    disk(move || {
        let changed = Devices::open(&server.data)
            .and_then(|devices| devices.set_retention(actor, policy.keep_count, policy.keep_days))
            .map_err(device_status)?;
        if !changed {
            return Err(StatusCode::FORBIDDEN);
        }
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "retention_changed", "actor": actor,
                "keep_count": policy.keep_count, "keep_days": policy.keep_days
            })
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

pub async fn observe(State(server): State<Shared>, request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("unmatched");
    let operation = format!("{} {route}", request.method());
    let response = next.run(request).await;
    if response.status().is_server_error() {
        server
            .diagnostics
            .error(operation, Some(response.status().as_u16()));
    }
    response
}

pub async fn openapi() -> impl axum::response::IntoResponse {
    (
        [("content-type", "application/yaml; charset=utf-8")],
        include_str!("../openapi.yaml"),
    )
}

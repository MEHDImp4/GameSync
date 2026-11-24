mod devices;
mod diagnostics;
mod limits;
mod transfers;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{delete, get, post, put},
};
use devices::{Credentials, Device, Devices, PairRequest};
use gamesync_core::{GameRegistration, Manifest};
use gamesync_storage::{
    ObjectError, ObjectStore, Publication, PublishInput, RevisionError, Revisions,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Seek, SeekFrom},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;

const MAX_OBJECT: usize = 8 * 1024 * 1024;

struct Server {
    data: PathBuf,
    uploads: std::sync::Mutex<()>,
    diagnostics: diagnostics::Diagnostics,
    retention: (u32, u32),
    limits: limits::Limits,
}

type Shared = Arc<Server>;
type HttpResult<T> = Result<T, StatusCode>;

impl Server {
    async fn authorize(&self, headers: &HeaderMap) -> HttpResult<Uuid> {
        let token = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| value.len() == 64)
            .ok_or(StatusCode::UNAUTHORIZED)?
            .to_owned();
        let data = self.data.clone();
        disk(move || {
            Devices::open(&data)
                .and_then(|devices| devices.authenticate(&token))
                .map_err(device_status)?
                .ok_or(StatusCode::UNAUTHORIZED)
        })
        .await
    }

    fn objects(&self) -> HttpResult<ObjectStore> {
        ObjectStore::open(self.data.join("objects")).map_err(object_status)
    }

    fn revisions(&self) -> HttpResult<Revisions> {
        Revisions::open(self.data.join("metadata.sqlite")).map_err(revision_status)
    }
}

fn object_status(error: ObjectError) -> StatusCode {
    match error {
        ObjectError::InvalidDigest => StatusCode::BAD_REQUEST,
        ObjectError::Integrity => StatusCode::UNPROCESSABLE_ENTITY,
        ObjectError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            StatusCode::NOT_FOUND
        }
        other => {
            eprintln!("object operation failed: {other}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn revision_status(error: RevisionError) -> StatusCode {
    match error {
        RevisionError::InvalidGame => StatusCode::BAD_REQUEST,
        RevisionError::GameRootsMismatch => StatusCode::CONFLICT,
        RevisionError::UnknownRevision => StatusCode::NOT_FOUND,
        RevisionError::ExpiredRevision => StatusCode::GONE,
        RevisionError::StaleResolution => StatusCode::PRECONDITION_FAILED,
        RevisionError::PublicationMismatch => StatusCode::CONFLICT,
        RevisionError::Object(error) => object_status(error),
        other => {
            eprintln!("revision operation failed: {other}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn disk<T: Send + 'static>(
    operation: impl FnOnce() -> HttpResult<T> + Send + 'static,
) -> HttpResult<T> {
    static DISK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    let slot = DISK
        .acquire()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        operation()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

async fn upload(
    State(server): State<Shared>,
    Path(digest): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> HttpResult<StatusCode> {
    server.authorize(&headers).await?;
    disk(move || {
        server
            .objects()?
            .put(&digest, bytes.len() as u64, &bytes[..])
            .map_err(object_status)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn download(
    State(server): State<Shared>,
    Path((digest, size)): Path<(String, u64)>,
    headers: HeaderMap,
) -> HttpResult<Response> {
    use tokio::io::AsyncReadExt;
    server.authorize(&headers).await?;
    if size > transfers::MAX_FILE {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut response = Response::builder().header(header::ACCEPT_RANGES, "bytes");
    let offset = if let Some(range) = headers.get(header::RANGE) {
        let offset = range
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|offset| *offset < size);
        let Some(offset) = offset else {
            return response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
        };
        response = response.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {offset}-{}/{size}", size - 1),
        );
        offset
    } else {
        0
    };
    let slot = server.limits.download_slot()?;
    let file = disk(move || {
        let mut file = server
            .objects()?
            .open_verified(&digest, size)
            .map_err(object_status)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(file)
    })
    .await?;
    let reader = tokio::fs::File::from_std(file).take(size - offset);
    let stream = tokio_util::io::ReaderStream::new(limits::Download::new(reader, slot));
    response
        .header(header::CONTENT_LENGTH, size - offset)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(stream))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishRequest {
    request: Uuid,
    base: Option<i64>,
    manifest: Manifest,
    resolves: Option<i64>,
}

#[derive(Serialize)]
struct PublishResponse {
    revision: i64,
    conflict: bool,
}

async fn publish(
    State(server): State<Shared>,
    Path(game): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PublishRequest>,
) -> HttpResult<(StatusCode, Json<PublishResponse>)> {
    let machine = server.authorize(&headers).await?;
    if body.manifest.entries().len() > 4096
        || body
            .manifest
            .entries()
            .iter()
            .any(|entry| entry.size > transfers::MAX_FILE)
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    disk(move || {
        match server
            .revisions()?
            .publish_with_resolution(
                &server.objects()?,
                PublishInput {
                    game,
                    machine,
                    request: body.request,
                    base: body.base,
                    manifest: &body.manifest,
                    resolves: body.resolves,
                },
            )
            .map_err(revision_status)?
        {
            Publication::Head(revision) => Ok((
                StatusCode::OK,
                Json(PublishResponse {
                    revision,
                    conflict: false,
                }),
            )),
            Publication::Conflict(revision) => Ok((
                StatusCode::CONFLICT,
                Json(PublishResponse {
                    revision,
                    conflict: true,
                }),
            )),
        }
    })
    .await
}

#[derive(Serialize)]
struct RevisionResponse {
    id: i64,
    machine: String,
    created_at: i64,
    manifest: Manifest,
    published: bool,
    resolved_by: Option<i64>,
}

async fn revision(
    State(server): State<Shared>,
    Path((game, id)): Path<(Uuid, i64)>,
    headers: HeaderMap,
) -> HttpResult<Json<RevisionResponse>> {
    server.authorize(&headers).await?;
    disk(move || {
        let revision = server.revisions()?.get(game, id).map_err(revision_status)?;
        Ok(Json(RevisionResponse {
            id: revision.id,
            machine: revision.machine,
            created_at: revision.created_at,
            manifest: revision.manifest,
            published: revision.published,
            resolved_by: revision.resolved_by,
        }))
    })
    .await
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<i64>,
}

#[derive(Serialize)]
struct HistoryItem {
    id: i64,
    machine: String,
    created_at: i64,
    published: bool,
    resolved_by: Option<i64>,
    bytes: u64,
}

async fn history(
    State(server): State<Shared>,
    Path(game): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
) -> HttpResult<Json<Vec<HistoryItem>>> {
    server.authorize(&headers).await?;
    disk(move || {
        let records = server
            .revisions()?
            .history(game, query.before.unwrap_or(i64::MAX))
            .map_err(revision_status)?;
        Ok(Json(
            records
                .into_iter()
                .map(|record| HistoryItem {
                    id: record.id,
                    machine: record.machine,
                    created_at: record.created_at,
                    published: record.published,
                    resolved_by: record.resolved_by,
                    bytes: record.bytes,
                })
                .collect(),
        ))
    })
    .await
}

async fn list_games(
    State(server): State<Shared>,
    headers: HeaderMap,
) -> HttpResult<Json<Vec<GameRegistration>>> {
    server.authorize(&headers).await?;
    disk(move || Ok(Json(server.revisions()?.games().map_err(revision_status)?))).await
}

async fn register_game(
    State(server): State<Shared>,
    Path(game): Path<Uuid>,
    headers: HeaderMap,
    Json(registration): Json<GameRegistration>,
) -> HttpResult<StatusCode> {
    server.authorize(&headers).await?;
    if game != registration.id {
        return Err(StatusCode::BAD_REQUEST);
    }
    disk(move || {
        server
            .revisions()?
            .register_game(&registration)
            .map_err(revision_status)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn head(
    State(server): State<Shared>,
    Path(game): Path<Uuid>,
    headers: HeaderMap,
) -> HttpResult<Json<Option<i64>>> {
    server.authorize(&headers).await?;
    disk(move || {
        Ok(Json(
            server.revisions()?.head(game).map_err(revision_status)?,
        ))
    })
    .await
}

fn device_status(error: rusqlite::Error) -> StatusCode {
    eprintln!("device operation failed: {error}");
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn pair(
    State(server): State<Shared>,
    Json(body): Json<PairRequest>,
) -> HttpResult<Json<Credentials>> {
    let name = body.name.trim();
    if name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || body.code.len() != 64
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    disk(move || {
        let credentials = Devices::open(&server.data)
            .and_then(|mut devices| devices.pair(&body.code, body.name.trim()))
            .map_err(device_status)?
            .ok_or(StatusCode::UNAUTHORIZED)?;
        eprintln!("device paired: {}", credentials.device);
        Ok(Json(credentials))
    })
    .await
}

#[derive(Serialize)]
struct DeviceList {
    current: Uuid,
    administrator: bool,
    items: Vec<Device>,
}

#[derive(Serialize)]
struct Invite {
    code: String,
}

async fn list_devices(
    State(server): State<Shared>,
    headers: HeaderMap,
) -> HttpResult<Json<DeviceList>> {
    let current = server.authorize(&headers).await?;
    disk(move || {
        let devices = Devices::open(&server.data).map_err(device_status)?;
        Ok(Json(DeviceList {
            current,
            administrator: devices.is_admin(current).map_err(device_status)?,
            items: devices.list().map_err(device_status)?,
        }))
    })
    .await
}

async fn invite_device(
    State(server): State<Shared>,
    headers: HeaderMap,
) -> HttpResult<Json<Invite>> {
    let actor = server.authorize(&headers).await?;
    disk(move || {
        let code = Devices::open(&server.data)
            .and_then(|mut devices| devices.invite_as(actor))
            .map_err(device_status)?
            .ok_or(StatusCode::FORBIDDEN)?;
        eprintln!("device invitation created by {actor}");
        Ok(Json(Invite { code }))
    })
    .await
}

async fn revoke_device(
    State(server): State<Shared>,
    Path(target): Path<Uuid>,
    headers: HeaderMap,
) -> HttpResult<StatusCode> {
    let actor = server.authorize(&headers).await?;
    disk(move || {
        match Devices::open(&server.data)
            .and_then(|mut devices| devices.revoke_as(actor, target))
            .map_err(device_status)?
        {
            None => Err(StatusCode::FORBIDDEN),
            Some(false) => Err(StatusCode::NOT_FOUND),
            Some(true) => {
                eprintln!("device {target} revoked by {actor}");
                Ok(StatusCode::NO_CONTENT)
            }
        }
    })
    .await
}

fn retention_settings() -> Result<(u32, u32), Box<dyn std::error::Error>> {
    let count = std::env::var("GAMESYNC_KEEP_COUNT")
        .unwrap_or_else(|_| "20".into())
        .parse::<u32>()?;
    let days = std::env::var("GAMESYNC_KEEP_DAYS")
        .unwrap_or_else(|_| "30".into())
        .parse::<u32>()?;
    if count == 0 || days == 0 {
        return Err("GAMESYNC_KEEP_COUNT and GAMESYNC_KEEP_DAYS must be positive".into());
    }
    Ok((count, days))
}

fn cleanup(data: &std::path::Path, defaults: (u32, u32)) -> Result<(), Box<dyn std::error::Error>> {
    let (count, days) = Devices::open(data)?.retention(defaults)?;
    let objects = ObjectStore::open(data.join("objects"))?;
    let result = Revisions::open(data.join("metadata.sqlite"))?.cleanup(
        &objects,
        count,
        days,
        std::time::Duration::from_secs(86400),
    )?;
    eprintln!(
        "retention expired_revisions={} removed_objects={}",
        result.expired_revisions, result.removed_objects
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = PathBuf::from(std::env::var_os("GAMESYNC_DATA").unwrap_or_else(|| "data".into()));
    std::fs::create_dir_all(&data)?;
    let mut devices = Devices::open(&data)?;
    devices.initialize()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [command] if command == "invite" => {
            println!("{}", devices.invite(false)?);
            return Ok(());
        }
        [command, flag] if command == "invite" && flag == "--admin" => {
            println!("{}", devices.invite(true)?);
            return Ok(());
        }
        [command, device] if command == "revoke" => {
            if !devices.revoke(Uuid::parse_str(device)?)? {
                return Err("unknown device".into());
            }
            eprintln!("device revoked: {device}");
            return Ok(());
        }
        [command] if command == "cleanup" => {
            cleanup(&data, retention_settings()?)?;
            return Ok(());
        }
        _ => {
            return Err(
                "usage: gamesync-server [invite [--admin] | revoke DEVICE_UUID | cleanup]".into(),
            );
        }
    }
    let retention = retention_settings()?;
    let server = Arc::new(Server {
        data,
        uploads: std::sync::Mutex::new(()),
        diagnostics: diagnostics::Diagnostics::default(),
        retention,
        limits: limits::Limits::default(),
    });
    Revisions::open(server.data.join("metadata.sqlite"))?;
    ObjectStore::open(server.data.join("objects"))?;
    let cleanup_server = server.clone();
    let router = Router::new()
        .route("/v1/status", get(diagnostics::status))
        .route(
            "/v1/retention",
            put(diagnostics::set_retention).layer(DefaultBodyLimit::max(256)),
        )
        .route("/openapi.yaml", get(diagnostics::openapi))
        .route("/v1/uploads", delete(transfers::cancel))
        .route("/v1/uploads/{digest}/{size}", get(transfers::status))
        .route(
            "/v1/uploads/{digest}/{size}/{offset}",
            put(transfers::append)
                .layer(DefaultBodyLimit::max(transfers::CHUNK))
                .layer(axum::middleware::from_fn_with_state(
                    server.clone(),
                    limits::upload_body,
                )),
        )
        .route("/v1/devices", get(list_devices))
        .route("/v1/devices/invitations", post(invite_device))
        .route("/v1/devices/{target}", delete(revoke_device))
        .route(
            "/v1/pair",
            post(pair).layer(DefaultBodyLimit::max(1024)).layer(
                axum::middleware::from_fn_with_state(server.clone(), limits::pairing),
            ),
        )
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/v1/objects/{digest}",
            put(upload).layer(DefaultBodyLimit::max(MAX_OBJECT)).layer(
                axum::middleware::from_fn_with_state(server.clone(), limits::upload_body),
            ),
        )
        .route("/v1/objects/{digest}/{size}", get(download))
        .route("/v1/games", get(list_games))
        .route(
            "/v1/games/{game}",
            put(register_game).layer(DefaultBodyLimit::max(8192)),
        )
        .route("/v1/games/{game}/head", get(head).post(publish))
        .route("/v1/games/{game}/revisions", get(history))
        .route("/v1/games/{game}/revisions/{id}", get(revision))
        .layer(axum::middleware::from_fn_with_state(
            server.clone(),
            limits::requests,
        ))
        .layer(axum::middleware::from_fn_with_state(
            server.clone(),
            diagnostics::observe,
        ))
        .with_state(server);
    let port = std::env::var("GAMESYNC_PORT")
        .unwrap_or_else(|_| "8787".into())
        .parse::<u16>()?;
    let bind = std::env::var("GAMESYNC_BIND")
        .unwrap_or_else(|_| "127.0.0.1".into())
        .parse::<std::net::IpAddr>()?;
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    eprintln!("GameSync listening on {}", listener.local_addr()?);
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(86400);
        let start = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut schedule = tokio::time::interval_at(start, period);
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            schedule.tick().await;
            let server = cleanup_server.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(error) = cleanup(&server.data, server.retention) {
                    server.diagnostics.error("retention".into(), None);
                    eprintln!("retention failed: {error}");
                }
                match transfers::prune_interrupted(&server) {
                    Ok(removed) if removed > 0 => {
                        eprintln!("expired interrupted uploads: {removed}")
                    }
                    Ok(_) => {}
                    Err(error) => {
                        server.diagnostics.error("upload retention".into(), None);
                        eprintln!("upload retention failed: {error}");
                    }
                }
            })
            .await;
        }
    });
    axum::serve(listener, router).await?;
    Ok(())
}

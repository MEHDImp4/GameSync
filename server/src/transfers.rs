use super::{HttpResult, Shared, disk, object_status};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use gamesync_storage::ObjectError;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
};
use uuid::Uuid;

pub const MAX_FILE: u64 = 1024 * 1024 * 1024;
pub const CHUNK: usize = 8 * 1024 * 1024;

#[derive(Serialize)]
pub struct Progress {
    offset: u64,
    complete: bool,
}

type TransferPath = (String, u64);
type AppendPath = (String, u64, u64);

fn validate(digest: &str, size: u64) -> HttpResult<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if size > MAX_FILE {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(())
}

fn file_error(error: io::Error) -> StatusCode {
    eprintln!("upload filesystem operation failed: {error}");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub async fn status(
    State(server): State<Shared>,
    Path((digest, size)): Path<TransferPath>,
    headers: HeaderMap,
) -> HttpResult<Json<Progress>> {
    let device = server.authorize(&headers).await?;
    validate(&digest, size)?;
    disk(move || update(&server, device, &digest, size, None).map(Json)).await
}

pub async fn append(
    State(server): State<Shared>,
    Path((digest, size, offset)): Path<AppendPath>,
    headers: HeaderMap,
    bytes: Bytes,
) -> HttpResult<Json<Progress>> {
    let device = server.authorize(&headers).await?;
    validate(&digest, size)?;
    if offset > size || bytes.len() as u64 > size - offset || (bytes.is_empty() && offset != size) {
        return Err(StatusCode::BAD_REQUEST);
    }
    disk(move || update(&server, device, &digest, size, Some((offset, bytes))).map(Json)).await
}

pub async fn cancel(State(server): State<Shared>, headers: HeaderMap) -> HttpResult<StatusCode> {
    let device = server.authorize(&headers).await?;
    disk(move || {
        let _guard = server
            .uploads
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let path = server.data.join("uploads").join(device.to_string());
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(StatusCode::NO_CONTENT),
            Err(error) => Err(file_error(error)),
        }
    })
    .await
}

fn update(
    server: &Shared,
    device: Uuid,
    digest: &str,
    size: u64,
    chunk: Option<(u64, Bytes)>,
) -> HttpResult<Progress> {
    let _guard = server
        .uploads
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let store = server.objects()?;
    let directory = server.data.join("uploads").join(device.to_string());
    let partial = directory.join(format!("{digest}.{size}"));
    match store.open_verified(digest, size) {
        Ok(_) => {
            if partial.exists() {
                fs::remove_file(&partial).map_err(file_error)?;
            }
            return Ok(Progress {
                offset: size,
                complete: true,
            });
        }
        Err(ObjectError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(object_status(error)),
    }

    fs::create_dir_all(&directory).map_err(file_error)?;
    if let Some(entry) = fs::read_dir(&directory).map_err(file_error)?.next()
        && entry.map_err(file_error)?.path() != partial
    {
        return Err(StatusCode::CONFLICT);
    }
    let mut offset = match fs::metadata(&partial) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(file_error(error)),
    };
    if let Some((expected, bytes)) = chunk {
        if expected != offset {
            return Err(StatusCode::CONFLICT);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial)
            .map_err(file_error)?;
        file.write_all(&bytes).map_err(file_error)?;
        file.sync_all().map_err(file_error)?;
        #[cfg(unix)]
        File::open(&directory)
            .and_then(|dir| dir.sync_all())
            .map_err(file_error)?;
        offset += bytes.len() as u64;
    }
    if offset == size && partial.exists() {
        let result = store.put(digest, size, File::open(&partial).map_err(file_error)?);
        if result.is_ok() || matches!(result, Err(ObjectError::Integrity)) {
            fs::remove_file(&partial).map_err(file_error)?;
        }
        result.map_err(object_status)?;
        return Ok(Progress {
            offset,
            complete: true,
        });
    }
    Ok(Progress {
        offset,
        complete: false,
    })
}

pub fn prune_interrupted(server: &Shared) -> io::Result<usize> {
    let _guard = server
        .uploads
        .lock()
        .map_err(|_| io::Error::other("upload lock poisoned"))?;
    prune_directory(
        &server.data.join("uploads"),
        std::time::SystemTime::now(),
        std::time::Duration::from_secs(7 * 86400),
    )
}

fn prune_directory(
    directory: &std::path::Path,
    now: std::time::SystemTime,
    max_idle: std::time::Duration,
) -> io::Result<usize> {
    let devices = match fs::read_dir(directory) {
        Ok(devices) => devices,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for device in devices {
        let device = device?;
        if !device.file_type()?.is_dir()
            || !device
                .file_name()
                .to_str()
                .is_some_and(|name| Uuid::parse_str(name).is_ok())
        {
            continue;
        }
        for transfer in fs::read_dir(device.path())? {
            let transfer = transfer?;
            let name = transfer.file_name();
            let Some((digest, size)) = name.to_str().and_then(|name| name.split_once('.')) else {
                continue;
            };
            if !transfer.file_type()?.is_file()
                || !size
                    .parse::<u64>()
                    .is_ok_and(|size| validate(digest, size).is_ok())
            {
                continue;
            }
            if now
                .duration_since(transfer.metadata()?.modified()?)
                .is_ok_and(|idle| idle >= max_idle)
            {
                fs::remove_file(transfer.path())?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn interrupted_transfer_cleanup_preserves_recent_and_unrecognized_files() {
        let root = tempfile::tempdir().unwrap();
        let device = root.path().join(Uuid::new_v4().to_string());
        fs::create_dir_all(&device).unwrap();
        let stale = device.join(format!("{}.100", "a".repeat(64)));
        let recent = device.join(format!("{}.100", "b".repeat(64)));
        let sentinel = device.join("unrecognized");
        let now = SystemTime::now();
        let old = now - Duration::from_secs(8 * 86400);
        for path in [&stale, &sentinel] {
            let file = File::create(path).unwrap();
            file.set_modified(old).unwrap();
        }
        File::create(&recent).unwrap().set_modified(now).unwrap();
        assert_eq!(
            prune_directory(root.path(), now, Duration::from_secs(7 * 86400)).unwrap(),
            1
        );
        assert!(!stale.exists());
        assert!(recent.is_file());
        assert!(sentinel.is_file());
    }
}

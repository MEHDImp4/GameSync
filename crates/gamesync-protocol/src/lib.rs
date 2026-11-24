use gamesync_core::{FileEntry, Manifest};
pub use gamesync_core::{GameRegistration, GameRoot};
use reqwest::{Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

mod transfer;
use transfer::Meter;
pub use transfer::{Direction, TransferOptions, TransferProgress};

pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("server address must use HTTPS or loopback HTTP")]
    Address,
    #[error("HTTP request failed: {0}")]
    Http(reqwest::Error),
    #[error("server returned HTTP {0}")]
    Status(StatusCode),
    #[error("invalid transfer state or file integrity check failed")]
    Integrity,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ClientError {
    pub fn is_stale_resolution(&self) -> bool {
        matches!(self, Self::Status(StatusCode::PRECONDITION_FAILED))
    }

    pub fn is_expired_revision(&self) -> bool {
        matches!(self, Self::Status(StatusCode::GONE))
    }
}

impl From<reqwest::Error> for ClientError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}

#[derive(Serialize)]
struct PairRequest<'a> {
    code: &'a str,
    name: &'a str,
}

#[derive(Deserialize)]
pub struct Credentials {
    pub device: Uuid,
    pub token: String,
}

#[derive(Deserialize, Serialize)]
pub struct ServerStatus {
    pub used_bytes: u64,
    pub objects: u64,
    pub synchronized_games: u64,
    pub keep_count: u32,
    pub keep_days: u32,
    pub latest: Vec<RecentPublication>,
    pub errors: Vec<RecentError>,
}

#[derive(Deserialize, Serialize)]
pub struct RecentPublication {
    pub revision: i64,
    pub game: Uuid,
    pub title: Option<String>,
    pub machine: Uuid,
    pub created_at: i64,
}

#[derive(Deserialize, Serialize)]
pub struct RecentError {
    pub time: u64,
    pub operation: String,
    pub status: Option<u16>,
}

#[derive(Deserialize, Serialize)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub administrator: bool,
    pub revoked: bool,
    pub last_seen: Option<i64>,
}

#[derive(Deserialize, Serialize)]
pub struct DeviceList {
    pub current: Uuid,
    pub administrator: bool,
    pub items: Vec<Device>,
}

#[derive(Deserialize)]
pub struct Revision {
    pub id: i64,
    pub machine: Uuid,
    pub created_at: i64,
    pub manifest: Manifest,
    pub published: bool,
    pub resolved_by: Option<i64>,
}

#[derive(Deserialize)]
pub struct RevisionSummary {
    pub id: i64,
    pub machine: Uuid,
    pub created_at: i64,
    pub published: bool,
    pub resolved_by: Option<i64>,
    pub bytes: u64,
}

#[derive(Serialize)]
struct PublishRequest<'a> {
    request: Uuid,
    base: Option<i64>,
    manifest: &'a Manifest,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolves: Option<i64>,
}

#[derive(Deserialize)]
pub struct Publication {
    pub revision: i64,
    pub conflict: bool,
}

#[derive(Deserialize)]
struct Progress {
    offset: u64,
    complete: bool,
}

pub struct Api {
    origin: Url,
    http: reqwest::Client,
}

pub struct Session {
    api: Api,
    credentials: Credentials,
    transfers: TransferOptions,
}

impl Api {
    pub fn new(address: &str) -> Result<Self, ClientError> {
        let origin = Url::parse(address).map_err(|_| ClientError::Address)?;
        let loopback = origin.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        });
        if !(origin.scheme() == "https" || origin.scheme() == "http" && loopback)
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(ClientError::Address);
        }
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30));
        let http = if loopback {
            builder.no_proxy()
        } else {
            builder
        }
        .build()?;
        Ok(Self { origin, http })
    }

    pub async fn pair(&self, code: &str, name: &str) -> Result<Credentials, ClientError> {
        Ok(checked(
            self.http
                .post(self.url("/v1/pair"))
                .json(&PairRequest { code, name })
                .send()
                .await?,
        )?
        .json()
        .await?)
    }

    pub fn authenticate(self, credentials: Credentials) -> Session {
        Session {
            api: self,
            credentials,
            transfers: TransferOptions::default(),
        }
    }

    fn url(&self, path: &str) -> Url {
        let mut url = self.origin.clone();
        url.set_path(path);
        url
    }
}

fn checked(response: Response) -> Result<Response, ClientError> {
    if !response.status().is_success() {
        return Err(ClientError::Status(response.status()));
    }
    Ok(response)
}

impl Session {
    pub fn with_transfer_options(mut self, options: TransferOptions) -> Self {
        self.transfers = options;
        self
    }

    pub fn device(&self) -> Uuid {
        self.credentials.device
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.api
            .http
            .request(method, self.api.url(path))
            .bearer_auth(&self.credentials.token)
    }

    pub async fn status(&self) -> Result<ServerStatus, ClientError> {
        Ok(checked(
            self.request(reqwest::Method::GET, "/v1/status")
                .send()
                .await?,
        )?
        .json()
        .await?)
    }

    pub async fn set_retention(&self, keep_count: u32, keep_days: u32) -> Result<(), ClientError> {
        #[derive(Serialize)]
        struct Retention {
            keep_count: u32,
            keep_days: u32,
        }
        checked(
            self.request(reqwest::Method::PUT, "/v1/retention")
                .json(&Retention {
                    keep_count,
                    keep_days,
                })
                .send()
                .await?,
        )?;
        Ok(())
    }

    pub async fn devices(&self) -> Result<DeviceList, ClientError> {
        Ok(checked(
            self.request(reqwest::Method::GET, "/v1/devices")
                .send()
                .await?,
        )?
        .json()
        .await?)
    }

    pub async fn invite_device(&self) -> Result<String, ClientError> {
        #[derive(Deserialize)]
        struct Invite {
            code: String,
        }
        let result: Invite = checked(
            self.request(reqwest::Method::POST, "/v1/devices/invitations")
                .send()
                .await?,
        )?
        .json()
        .await?;
        Ok(result.code)
    }

    pub async fn revoke_device(&self, device: Uuid) -> Result<(), ClientError> {
        checked(
            self.request(reqwest::Method::DELETE, &format!("/v1/devices/{device}"))
                .send()
                .await?,
        )?;
        Ok(())
    }

    pub async fn games(&self) -> Result<Vec<GameRegistration>, ClientError> {
        Ok(checked(
            self.request(reqwest::Method::GET, "/v1/games")
                .send()
                .await?,
        )?
        .json()
        .await?)
    }

    pub async fn register_game(&self, game: &GameRegistration) -> Result<(), ClientError> {
        checked(
            self.request(reqwest::Method::PUT, &format!("/v1/games/{}", game.id))
                .json(game)
                .send()
                .await?,
        )?;
        Ok(())
    }

    pub async fn head(&self, game: Uuid) -> Result<Option<i64>, ClientError> {
        Ok(checked(
            self.request(reqwest::Method::GET, &format!("/v1/games/{game}/head"))
                .send()
                .await?,
        )?
        .json()
        .await?)
    }

    pub async fn revision(&self, game: Uuid, id: i64) -> Result<Revision, ClientError> {
        Ok(checked(
            self.request(
                reqwest::Method::GET,
                &format!("/v1/games/{game}/revisions/{id}"),
            )
            .send()
            .await?,
        )?
        .json()
        .await?)
    }

    pub async fn history(
        &self,
        game: Uuid,
        before: Option<i64>,
    ) -> Result<Vec<RevisionSummary>, ClientError> {
        let mut request =
            self.request(reqwest::Method::GET, &format!("/v1/games/{game}/revisions"));
        if let Some(before) = before {
            request = request.query(&[("before", before)]);
        }
        Ok(checked(request.send().await?)?.json().await?)
    }

    pub async fn publish(
        &self,
        game: Uuid,
        request: Uuid,
        base: Option<i64>,
        manifest: &Manifest,
    ) -> Result<Publication, ClientError> {
        self.publish_resolving(game, request, base, manifest, None)
            .await
    }

    pub async fn publish_resolving(
        &self,
        game: Uuid,
        request: Uuid,
        base: Option<i64>,
        manifest: &Manifest,
        resolves: Option<i64>,
    ) -> Result<Publication, ClientError> {
        let response = self
            .request(reqwest::Method::POST, &format!("/v1/games/{game}/head"))
            .json(&PublishRequest {
                request,
                base,
                manifest,
                resolves,
            })
            .send()
            .await?;
        let conflict = response.status() == StatusCode::CONFLICT;
        if !conflict && !response.status().is_success() {
            return Err(ClientError::Status(response.status()));
        }
        if conflict
            && !response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .is_some_and(|header| header.as_bytes().starts_with(b"application/json"))
        {
            return Err(ClientError::Status(StatusCode::CONFLICT));
        }
        let publication: Publication = response.json().await?;
        if publication.conflict != conflict {
            return Err(ClientError::Integrity);
        }
        Ok(publication)
    }

    fn object_route(prefix: &str, entry: &FileEntry) -> Result<String, ClientError> {
        if blake3::Hash::from_hex(&entry.digest).is_err() {
            return Err(ClientError::Integrity);
        }
        Ok(format!("/v1/{prefix}/{}/{}", entry.digest, entry.size))
    }

    pub async fn upload(&self, entry: &FileEntry, source: &Path) -> Result<(), ClientError> {
        self.upload_file(entry, tokio::fs::File::open(source).await?)
            .await
    }

    pub async fn upload_file(
        &self,
        entry: &FileEntry,
        mut file: tokio::fs::File,
    ) -> Result<(), ClientError> {
        let path = Self::object_route("uploads", entry)?;
        if file.metadata().await?.len() != entry.size {
            return Err(ClientError::Integrity);
        }
        let mut progress: Progress =
            checked(self.request(reqwest::Method::GET, &path).send().await?)?
                .json()
                .await?;
        if progress.offset > entry.size || (progress.complete && progress.offset != entry.size) {
            return Err(ClientError::Integrity);
        }
        file.seek(std::io::SeekFrom::Start(progress.offset)).await?;
        let maximum = self
            .transfers
            .bytes_per_second
            .map_or(CHUNK_SIZE as u64, |limit| {
                (limit.get() / 4).clamp(4096, CHUNK_SIZE as u64)
            });
        let mut meter = Meter::new(
            &self.transfers,
            Direction::Upload,
            &entry.path,
            progress.offset,
            entry.size,
        );
        if !progress.complete {
            meter.report(progress.offset);
        }
        while !progress.complete {
            let size = (entry.size - progress.offset).min(maximum) as usize;
            let mut chunk = vec![0; size];
            file.read_exact(&mut chunk).await?;
            let expected = progress.offset + size as u64;
            meter.pace(expected).await;
            progress = checked(
                self.request(reqwest::Method::PUT, &format!("{path}/{}", progress.offset))
                    .body(chunk)
                    .send()
                    .await?,
            )?
            .json()
            .await?;
            if progress.offset != expected || progress.complete != (expected == entry.size) {
                return Err(ClientError::Integrity);
            }
            meter.report(progress.offset);
        }
        Ok(())
    }

    /// A resumable staging file in a private cache. Only use its contents on success.
    pub async fn download(&self, entry: &FileEntry, staging: &Path) -> Result<(), ClientError> {
        let path = Self::object_route("objects", entry)?;
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(staging)
            .await?;
        let mut offset = file.metadata().await?.len();
        if offset > entry.size {
            file.set_len(0).await?;
            offset = 0;
        }

        let meter_started = offset < entry.size;
        let mut meter = Meter::new(
            &self.transfers,
            Direction::Download,
            &entry.path,
            offset,
            entry.size,
        );
        if offset < entry.size {
            meter.report(offset);
            let mut request = self.request(reqwest::Method::GET, &path);
            if offset != 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = checked(request.send().await?)?;
            if offset == 0 && response.status() != StatusCode::OK
                || offset != 0
                    && (response.status() != StatusCode::PARTIAL_CONTENT
                        || response
                            .headers()
                            .get(reqwest::header::CONTENT_RANGE)
                            .and_then(|value| value.to_str().ok())
                            != Some(&format!("bytes {offset}-{}/{}", entry.size - 1, entry.size)))
            {
                return Err(ClientError::Integrity);
            }
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            while let Some(chunk) = response.chunk().await? {
                if chunk.len() as u64 > entry.size - offset {
                    return Err(ClientError::Integrity);
                }
                let next = offset + chunk.len() as u64;
                meter.pace(next).await;
                file.write_all(&chunk).await?;
                offset = next;
                if offset < entry.size {
                    meter.report(offset);
                }
            }
        }

        if offset != entry.size {
            return Err(ClientError::Integrity);
        }
        file.seek(std::io::SeekFrom::Start(0)).await?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if hasher.finalize().to_hex().as_str() != entry.digest {
            file.set_len(0).await?;
            return Err(ClientError::Integrity);
        }
        file.sync_all().await?;
        if meter_started {
            meter.report(offset);
        }
        Ok(())
    }
}

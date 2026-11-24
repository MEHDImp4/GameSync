use crate::Shared;
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
};

const PAIR_ATTEMPTS: usize = 30;
const PAIR_WINDOW: Duration = Duration::from_secs(60);

pub struct Limits {
    requests: Semaphore,
    uploads: Semaphore,
    downloads: Arc<Semaphore>,
    pairing: Mutex<VecDeque<Instant>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            requests: Semaphore::new(24),
            uploads: Semaphore::new(4),
            downloads: Arc::new(Semaphore::new(8)),
            pairing: Mutex::default(),
        }
    }
}

impl Limits {
    pub fn allow_pairing(&self) -> bool {
        let Ok(mut attempts) = self.pairing.lock() else {
            return false;
        };
        let now = Instant::now();
        while attempts
            .front()
            .is_some_and(|start| now.duration_since(*start) >= PAIR_WINDOW)
        {
            attempts.pop_front();
        }
        if attempts.len() >= PAIR_ATTEMPTS {
            return false;
        }
        attempts.push_back(now);
        true
    }

    pub fn download_slot(&self) -> Result<OwnedSemaphorePermit, StatusCode> {
        self.downloads
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }
}

pub async fn requests(State(server): State<Shared>, request: Request, next: Next) -> Response {
    if matches!(request.uri().path(), "/healthz" | "/openapi.yaml") {
        return next.run(request).await;
    }
    let Ok(_permit) = server.limits.requests.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "2")],
        )
            .into_response();
    };
    next.run(request).await
}

pub async fn pairing(State(server): State<Shared>, request: Request, next: Next) -> Response {
    if !server.limits.allow_pairing() {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")]).into_response();
    }
    match tokio::time::timeout(Duration::from_secs(10), next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

pub async fn upload_body(State(server): State<Shared>, request: Request, next: Next) -> Response {
    let Ok(_permit) = server.limits.uploads.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "2")],
        )
            .into_response();
    };
    match tokio::time::timeout(Duration::from_secs(60), next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

/// Hold the stream slot until the body is dropped or reaches EOF.
pub struct Download<T> {
    reader: T,
    _slot: OwnedSemaphorePermit,
}

impl<T> Download<T> {
    pub fn new(reader: T, slot: OwnedSemaphorePermit) -> Self {
        Self {
            reader,
            _slot: slot,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Download<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(context, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_are_limited_until_a_stream_is_dropped() {
        let limits = Limits::default();
        let slots = (0..8)
            .map(|_| Download::new(tokio::io::empty(), limits.download_slot().unwrap()))
            .collect::<Vec<_>>();
        assert!(limits.download_slot().is_err());
        drop(slots);
        assert!(limits.download_slot().is_ok());
    }

    #[test]
    fn pairing_budget_reopens_after_the_window() {
        let limits = Limits::default();
        for _ in 0..PAIR_ATTEMPTS {
            assert!(limits.allow_pairing());
        }
        assert!(!limits.allow_pairing());
        let before = Instant::now() - PAIR_WINDOW;
        for timestamp in limits.pairing.lock().unwrap().iter_mut() {
            *timestamp = before;
        }
        assert!(limits.allow_pairing());
    }
}

use serde::Serialize;
use std::{
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Serialize)]
pub struct TransferProgress {
    pub direction: Direction,
    pub path: String,
    pub transferred: u64,
    pub total: u64,
    pub bytes_per_second: u64,
}

#[derive(Default)]
pub struct TransferOptions {
    pub bytes_per_second: Option<NonZeroU64>,
    pub observer: Option<Arc<dyn Fn(TransferProgress) + Send + Sync>>,
}

pub(crate) struct Meter<'a> {
    options: &'a TransferOptions,
    direction: Direction,
    path: &'a str,
    total: u64,
    initial: u64,
    started: Instant,
    reported: Option<Instant>,
}

impl<'a> Meter<'a> {
    pub(crate) fn new(
        options: &'a TransferOptions,
        direction: Direction,
        path: &'a str,
        initial: u64,
        total: u64,
    ) -> Self {
        Self {
            options,
            direction,
            path,
            total,
            initial,
            started: Instant::now(),
            reported: None,
        }
    }

    /// Pace payload bytes relative to the beginning of this transfer, excluding resumed bytes.
    pub(crate) async fn pace(&self, next_offset: u64) {
        if let Some(rate) = self.options.bytes_per_second {
            let elapsed = self.started.elapsed();
            let seconds = next_offset.saturating_sub(self.initial) as f64 / rate.get() as f64;
            if let Ok(budget) = Duration::try_from_secs_f64(seconds) {
                tokio::time::sleep(budget.saturating_sub(elapsed)).await;
            }
        }
    }

    pub(crate) fn report(&mut self, offset: u64) {
        let Some(observer) = &self.options.observer else {
            return;
        };
        if offset != self.total
            && self
                .reported
                .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.reported = Some(Instant::now());
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        observer(TransferProgress {
            direction: self.direction,
            path: self.path.into(),
            transferred: offset,
            total: self.total,
            bytes_per_second: (offset.saturating_sub(self.initial) as f64 / elapsed) as u64,
        });
    }
}

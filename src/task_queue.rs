use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use crate::{CaptureError, CaptureOptions, CaptureOutput};

pub(crate) type JobResult = Result<CaptureOutput, CaptureError>;

pub(crate) struct Job {
    pub id: u64,
    pub spec: CaptureOptions,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
    pub tx: tokio::sync::oneshot::Sender<JobResult>,
}

#[derive(Default)]
struct Inner {
    lanes: BTreeMap<(u8, u64), Job>,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct TaskQueue {
    inner: Mutex<Inner>,
    wake: Condvar,
    seq: AtomicU64,
}

impl TaskQueue {
    pub(crate) fn push(&self, rank: u8, job: Job) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.closed {
            drop(inner);
            drop(job.tx.send(Err(CaptureError::Render(
                "worker pool is unavailable".into(),
            ))));
            return;
        }
        inner
            .lanes
            .insert((rank, self.seq.fetch_add(1, Ordering::Relaxed)), job);
        drop(inner);
        self.wake.notify_one();
    }

    pub(crate) fn pop(&self) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some((_, job)) = inner.lanes.pop_first() {
                return Some(job);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .wake
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(crate) fn cancel_queued(&self, id: u64) -> bool {
        let job = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let key = inner
                .lanes
                .iter()
                .find(|(_, job)| job.id == id)
                .map(|(key, _)| *key);
            let job = key.and_then(|key| inner.lanes.remove(&key));
            drop(inner);
            job
        };
        if let Some(job) = job {
            drop(job.tx.send(Err(CaptureError::Cancelled)));
            return true;
        }
        false
    }

    pub(crate) fn fail_all(&self, message: String) {
        let jobs = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.closed = true;
            std::mem::take(&mut inner.lanes)
        };
        for job in jobs.into_values() {
            drop(job.tx.send(Err(CaptureError::Render(message.clone()))));
        }
        self.wake.notify_all();
    }

    pub(crate) fn close(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.wake.notify_all();
    }
}

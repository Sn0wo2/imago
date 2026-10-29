use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::render::CaptureOutput;
use crate::{CaptureError, CaptureOptions};

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

pub(crate) struct TaskQueue {
    inner: Mutex<Inner>,
    wake: Condvar,
    seq: AtomicU64,
}

impl TaskQueue {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            wake: Condvar::new(),
            seq: AtomicU64::new(0),
        }
    }

    pub(crate) fn push(&self, rank: u8, job: Job) {
        self.inner
            .lock()
            .unwrap()
            .lanes
            .insert((rank, self.seq.fetch_add(1, Ordering::Relaxed)), job);
        self.wake.notify_one();
    }

    pub(crate) fn pop(&self) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some((_, job)) = inner.lanes.pop_first() {
                return Some(job);
            }
            if inner.closed {
                return None;
            }
            inner = self.wake.wait(inner).unwrap();
        }
    }

    pub(crate) fn cancel_queued(&self, id: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let key = inner
            .lanes
            .iter()
            .find(|(_, job)| job.id == id)
            .map(|(k, _)| *k);
        if let Some(job) = key.and_then(|k| inner.lanes.remove(&k)) {
            let _ = job.tx.send(Err(CaptureError::Cancelled));
            return true;
        }
        false
    }

    pub(crate) fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.wake.notify_all();
    }
}

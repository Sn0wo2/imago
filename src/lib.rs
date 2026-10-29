#![allow(clippy::pub_use)]

mod render;
mod task_queue;
pub mod utils;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use blitz_net::Provider;
use tokio::sync::oneshot;

pub use blitz_traits::shell::ColorScheme;
pub use peniko::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::exhaustive_enums)]
pub enum ImageFormat {
    Png,
    Jpeg,
    WebP,
    Qoi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::exhaustive_enums)]
pub enum PngCompression {
    Fast,
    Default,
    Best,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::exhaustive_enums)]
pub enum Encoding {
    Png(PngCompression),
    Jpeg { quality: u8 },
    WebP { quality: Option<u8> },
    Qoi,
}

#[cfg(feature = "template")]
pub use minijinja as template;

#[derive(Debug)]
#[allow(clippy::exhaustive_structs)]
pub struct CaptureOutput {
    pub data: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub total_ms: f64,
    pub layout_ms: f64,
    pub render_ms: f64,
    pub encode_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(clippy::exhaustive_enums)]
pub enum Priority {
    Urgent,
    High,
    Normal,
    Low,
}

#[derive(Debug)]
#[allow(clippy::exhaustive_enums)]
pub enum CaptureError {
    Timeout,
    Cancelled,
    Render(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "capture timed out"),
            Self::Cancelled => write!(f, "capture cancelled"),
            Self::Render(e) => write!(f, "capture failed: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

#[derive(Debug)]
#[allow(clippy::exhaustive_structs)]
pub struct CaptureOptions {
    pub html: String,
    pub base_url: Option<String>,
    pub size: CaptureSize,
    pub pixel_ratio: f32,
    pub encoding: Encoding,
    pub background: Option<Color>,
    pub priority: Priority,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::exhaustive_enums)]
pub enum CaptureSize {
    Viewport { width: u32, height: u32 },
    FullPage { width: u32, max_height: Option<u32> },
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::exhaustive_structs)]
pub struct ImagoConfig {
    pub workers: usize,
    pub asset_timeout: Duration,
    pub color_scheme: ColorScheme,
    pub renderer_cache_capacity: usize,
}

impl Default for ImagoConfig {
    fn default() -> Self {
        Self {
            workers: 1,
            asset_timeout: Duration::from_secs(30),
            color_scheme: ColorScheme::Light,
            renderer_cache_capacity: 4,
        }
    }
}

impl CaptureOptions {
    #[cfg(feature = "template")]
    pub fn from_template(
        template: &template::Template<'_, '_>,
        context: impl serde::Serialize,
    ) -> Result<Self, template::Error> {
        template
            .render(template::value::Serde(context))
            .map(Self::new)
    }

    pub fn new(html: impl Into<String>) -> Self {
        Self {
            html: html.into(),
            base_url: None,
            size: CaptureSize::Viewport {
                width: 1024,
                height: 1880,
            },
            pixel_ratio: 2.0,
            encoding: Encoding::Png(PngCompression::Default),
            background: None,
            priority: Priority::Normal,
            timeout: Duration::from_secs(30),
        }
    }
}

struct Inner {
    task_queue: Arc<task_queue::TaskQueue>,
    next_id: AtomicU64,
    _worker_guards: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task_queue.close();
    }
}

#[derive(Clone)]
pub struct Imago {
    inner: Arc<Inner>,
}

pub struct JobTicket {
    id: u64,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    task_queue: Arc<task_queue::TaskQueue>,
    rx: oneshot::Receiver<task_queue::JobResult>,
}

impl fmt::Debug for Imago {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Imago").finish_non_exhaustive()
    }
}

impl fmt::Debug for JobTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobTicket")
            .field("id", &self.id)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl Imago {
    pub fn spawn(workers: usize) -> Self {
        Self::spawn_with_config(ImagoConfig {
            workers,
            ..Default::default()
        })
    }

    pub fn spawn_with_config(config: ImagoConfig) -> Self {
        let task_queue = Arc::new(task_queue::TaskQueue::default());
        let mut worker_guards = Vec::with_capacity(config.workers.max(1));
        for _ in 0..config.workers.max(1) {
            let worker_queue = Arc::clone(&task_queue);
            let worker = std::thread::Builder::new()
                .name("imago-worker".into())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            worker_queue.fail_all(format!("worker runtime failed: {error}"));
                            return;
                        }
                    };
                    let mut pipeline = render::Pipeline {
                        config,
                        net: Arc::new(Provider::new(None)),
                        renderers: HashMap::new(),
                        order: VecDeque::new(),
                    };
                    runtime.block_on(async {
                        while let Some(job) = worker_queue.pop() {
                            let result = if job.cancelled.load(Ordering::Relaxed) {
                                Err(CaptureError::Cancelled)
                            } else if Instant::now() >= job.deadline {
                                Err(CaptureError::Timeout)
                            } else {
                                tokio::time::timeout_at(
                                    job.deadline.into(),
                                    pipeline.capture(&job.spec),
                                )
                                .await
                                .unwrap_or(Err(CaptureError::Timeout))
                            };
                            drop(job.tx.send(
                                if job.cancelled.load(Ordering::Relaxed)
                                    && !matches!(result, Err(CaptureError::Timeout))
                                {
                                    Err(CaptureError::Cancelled)
                                } else {
                                    result
                                },
                            ));
                        }
                    });
                });
            match worker {
                Ok(guard) => worker_guards.push(guard),
                Err(error) if worker_guards.is_empty() => {
                    task_queue.fail_all(format!("failed to spawn imago worker: {error}"));
                }
                Err(_) => {}
            }
        }
        Self {
            inner: Arc::new(Inner {
                _worker_guards: worker_guards,
                task_queue,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn submit(&self, spec: CaptureOptions) -> JobTicket {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (tx, rx) = oneshot::channel();
        let deadline = Instant::now() + spec.timeout;
        let job = task_queue::Job {
            id,
            spec,
            deadline,
            cancelled: Arc::clone(&cancelled),
            tx,
        };
        self.inner.task_queue.push(job.spec.priority as u8, job);
        JobTicket {
            id,
            deadline,
            cancelled,
            task_queue: Arc::clone(&self.inner.task_queue),
            rx,
        }
    }
}

impl JobTicket {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.task_queue.cancel_queued(self.id);
    }

    pub async fn result(mut self) -> Result<CaptureOutput, CaptureError> {
        tokio::select! {
            res = &mut self.rx => res.unwrap_or_else(|_| {
                Err(CaptureError::Render("capture worker dropped the job".into()))
            }),
            _ = tokio::time::sleep_until(self.deadline.into()) => {
                self.cancel();
                Err(CaptureError::Timeout)
            }
        }
    }
}

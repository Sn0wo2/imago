mod encode;
mod render;
mod task_queue;
pub mod utils;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

pub use blitz_traits::shell::ColorScheme;
pub use encode::{Encoding, ImageFormat, PngCompression};
#[cfg(feature = "template")]
pub use minijinja as template;
pub use peniko::Color;
pub use render::CaptureOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Urgent,
    High,
    Normal,
    Low,
}

#[derive(Debug)]
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
pub enum CaptureSize {
    Viewport { width: u32, height: u32 },
    FullPage { width: u32, max_height: Option<u32> },
}

#[derive(Debug, Clone, Copy)]
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

impl Imago {
    pub fn spawn(workers: usize) -> Self {
        Self::spawn_with_config(ImagoConfig {
            workers,
            ..Default::default()
        })
    }

    pub fn spawn_with_config(config: ImagoConfig) -> Self {
        let task_queue = Arc::new(task_queue::TaskQueue::new());
        Self {
            inner: Arc::new(Inner {
                _worker_guards: (0..config.workers.max(1))
                    .map(|_| {
                        let task_queue = Arc::clone(&task_queue);
                        std::thread::Builder::new()
                            .name("imago-worker".into())
                            .spawn(move || {
                                tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .expect("worker runtime")
                                    .block_on(async {
                                        let mut pipeline = render::Pipeline::new(config);
                                        while let Some(job) = task_queue.pop() {
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
                                            let _ = job.tx.send(
                                                if job.cancelled.load(Ordering::Relaxed)
                                                    && !matches!(result, Err(CaptureError::Timeout))
                                                {
                                                    Err(CaptureError::Cancelled)
                                                } else {
                                                    result
                                                },
                                            );
                                        }
                                    });
                            })
                            .expect("spawn imago worker")
                    })
                    .collect(),
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
        self.inner.task_queue.push(
            spec.priority as u8,
            task_queue::Job {
                id,
                spec,
                deadline,
                cancelled: Arc::clone(&cancelled),
                tx,
            },
        );
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

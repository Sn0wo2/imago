use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use anyrender::{ImageRenderer, PaintScene as _};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_net::Provider;
use blitz_paint::paint_scene;
use blitz_traits::shell::Viewport;
use peniko::{Fill, kurbo::Rect};

use crate::{
    CaptureError, CaptureOptions, CaptureSize, Encoding, ImageFormat, ImagoConfig, encode,
};

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

pub(crate) struct Pipeline {
    config: ImagoConfig,
    net: Arc<Provider>,
    renderers: HashMap<(u32, u32), VelloCpuImageRenderer>,
    order: VecDeque<(u32, u32)>,
}

impl Pipeline {
    pub(crate) fn new(config: ImagoConfig) -> Self {
        Pipeline {
            config,
            net: Arc::new(Provider::new(None)),
            renderers: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(crate) async fn capture(
        &mut self,
        spec: &CaptureOptions,
    ) -> Result<CaptureOutput, CaptureError> {
        let t0 = Instant::now();

        let dev_w = match spec.size {
            CaptureSize::Viewport { width, .. } | CaptureSize::FullPage { width, .. } => {
                (width as f32 * spec.pixel_ratio).ceil().max(1.0) as u32
            }
        };
        let provisional_h = match spec.size {
            CaptureSize::Viewport { height, .. } => {
                (height as f32 * spec.pixel_ratio).ceil().max(1.0) as u32
            }
            CaptureSize::FullPage { .. } => 1,
        };

        let mut doc = HtmlDocument::from_html(
            &spec.html,
            DocumentConfig {
                base_url: spec.base_url.clone(),
                net_provider: Some(Arc::clone(&self.net) as _),
                viewport: Some(Viewport::new(
                    dev_w,
                    provisional_h,
                    spec.pixel_ratio,
                    self.config.color_scheme,
                )),
                ..Default::default()
            },
        );

        let start = Instant::now();
        loop {
            doc.resolve(0.0);
            if self.net.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
            if start.elapsed() >= self.config.asset_timeout {
                return Err(CaptureError::Render("asset fetch timeout".into()));
            }
        }
        doc.resolve(0.0);

        let dev_h = match spec.size {
            CaptureSize::Viewport { height, .. } => {
                (height as f32 * spec.pixel_ratio).ceil().max(1.0) as u32
            }
            CaptureSize::FullPage { max_height, .. } => ((doc.root_element().scroll_height()
                * spec.pixel_ratio)
                .ceil()
                .max(1.0) as u32)
                .min(max_height.map_or(u32::MAX, |cap| {
                    (cap as f32 * spec.pixel_ratio).ceil().max(1.0) as u32
                })),
        };
        if dev_h != provisional_h {
            doc.set_viewport(Viewport::new(
                dev_w,
                dev_h,
                spec.pixel_ratio,
                self.config.color_scheme,
            ));
            doc.resolve(0.0);
        }
        let t1 = Instant::now();

        let mut uncached_renderer;
        let renderer = if self.config.renderer_cache_capacity == 0 {
            uncached_renderer = VelloCpuImageRenderer::new(dev_w, dev_h);
            &mut uncached_renderer
        } else {
            if !self.renderers.contains_key(&(dev_w, dev_h)) {
                if self.order.len() >= self.config.renderer_cache_capacity
                    && let Some(oldest) = self.order.pop_front()
                {
                    self.renderers.remove(&oldest);
                }
                self.order.push_back((dev_w, dev_h));
                self.renderers
                    .insert((dev_w, dev_h), VelloCpuImageRenderer::new(dev_w, dev_h));
            }
            self.renderers.get_mut(&(dev_w, dev_h)).unwrap()
        };
        renderer.reset();

        let mut buf = Vec::with_capacity((dev_w * dev_h * 4) as usize);
        renderer.render_to_vec(
            |scene| {
                if let Some(color) = spec.background {
                    scene.fill(
                        Fill::NonZero,
                        Default::default(),
                        color,
                        Default::default(),
                        &Rect::new(0.0, 0.0, dev_w as f64, dev_h as f64),
                    );
                }
                paint_scene(scene, &mut doc, spec.pixel_ratio as f64, dev_w, dev_h, 0, 0);
            },
            &mut buf,
        );
        let t2 = Instant::now();

        let data = encode::encode(&buf, dev_w, dev_h, &spec.encoding, spec.background)
            .map_err(CaptureError::Render)?;
        let t3 = Instant::now();
        Ok(CaptureOutput {
            data,
            format: match spec.encoding {
                Encoding::Png(_) => ImageFormat::Png,
                Encoding::Jpeg { .. } => ImageFormat::Jpeg,
                Encoding::WebP { .. } => ImageFormat::WebP,
                Encoding::Qoi => ImageFormat::Qoi,
            },
            width: dev_w,
            height: dev_h,
            total_ms: (t3 - t0).as_secs_f64() * 1000.0,
            layout_ms: (t1 - t0).as_secs_f64() * 1000.0,
            render_ms: (t2 - t1).as_secs_f64() * 1000.0,
            encode_ms: (t3 - t2).as_secs_f64() * 1000.0,
        })
    }
}

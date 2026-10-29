use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
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
    CaptureError, CaptureOptions, CaptureOutput, CaptureSize, Encoding, ImageFormat, ImagoConfig,
    PngCompression,
};

pub(crate) struct Pipeline {
    pub(crate) config: ImagoConfig,
    pub(crate) net: Arc<Provider>,
    pub(crate) renderers: HashMap<(u32, u32), VelloCpuImageRenderer>,
    pub(crate) order: VecDeque<(u32, u32)>,
}

impl Pipeline {
    pub(crate) async fn capture(
        &mut self,
        spec: &CaptureOptions,
    ) -> Result<CaptureOutput, CaptureError> {
        let t0 = Instant::now();

        let dev_w = match spec.size {
            CaptureSize::Viewport { width, .. } | CaptureSize::FullPage { width, .. } => {
                scaled_pixels(width as f32, spec.pixel_ratio)
            }
        };
        let provisional_h = match spec.size {
            CaptureSize::Viewport { height, .. } => scaled_pixels(height as f32, spec.pixel_ratio),
            CaptureSize::FullPage { .. } => 1,
        };

        let mut doc = HtmlDocument::from_html(
            &spec.html,
            DocumentConfig {
                base_url: spec.base_url.clone(),
                net_provider: Some(self.net.clone()),
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
            CaptureSize::Viewport { height, .. } => scaled_pixels(height as f32, spec.pixel_ratio),
            CaptureSize::FullPage { max_height, .. } => {
                scaled_pixels(doc.root_element().scroll_height(), spec.pixel_ratio).min(
                    max_height.map_or(u32::MAX, |cap| scaled_pixels(cap as f32, spec.pixel_ratio)),
                )
            }
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
        let key = (dev_w, dev_h);
        let renderer = if self.config.renderer_cache_capacity == 0 {
            uncached_renderer = VelloCpuImageRenderer::new(dev_w, dev_h);
            &mut uncached_renderer
        } else {
            if !self.renderers.contains_key(&key) {
                if self.order.len() >= self.config.renderer_cache_capacity
                    && let Some(oldest) = self.order.pop_front()
                {
                    self.renderers.remove(&oldest);
                }
                self.order.push_back(key);
            }
            self.renderers
                .entry(key)
                .or_insert_with(|| VelloCpuImageRenderer::new(dev_w, dev_h))
        };
        renderer.reset();

        let mut buf = Vec::with_capacity((dev_w * dev_h * 4) as usize);
        renderer.render_to_vec(
            |scene| {
                if let Some(color) = spec.background {
                    scene.fill(
                        Fill::NonZero,
                        peniko::kurbo::Affine::default(),
                        color,
                        Option::default(),
                        &Rect::new(0.0, 0.0, f64::from(dev_w), f64::from(dev_h)),
                    );
                }
                paint_scene(
                    scene,
                    &mut doc,
                    f64::from(spec.pixel_ratio),
                    dev_w,
                    dev_h,
                    0,
                    0,
                );
            },
            &mut buf,
        );
        let t2 = Instant::now();

        let pixels = match (
            &spec.encoding,
            buf.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255),
        ) {
            (_, true) => Cow::Borrowed(buf.as_slice()),
            (Encoding::Jpeg { .. }, false) => {
                let [br, bg, bb, _] = spec
                    .background
                    .unwrap_or(crate::Color::new([1.0, 1.0, 1.0, 1.0]))
                    .components
                    .map(|channel| {
                        let channel = (channel * 255.0).round();
                        if channel <= 0.0 {
                            0
                        } else {
                            u32::try_from(channel as i64).unwrap_or(u32::MAX)
                        }
                    });
                Cow::Owned(
                    buf.as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|pixel| {
                            [
                                (u32::from(pixel[0]) + br * (255 - u32::from(pixel[3])) / 255)
                                    .min(255) as u8,
                                (u32::from(pixel[1]) + bg * (255 - u32::from(pixel[3])) / 255)
                                    .min(255) as u8,
                                (u32::from(pixel[2]) + bb * (255 - u32::from(pixel[3])) / 255)
                                    .min(255) as u8,
                                255,
                            ]
                        })
                        .collect(),
                )
            }
            (_, false) => Cow::Owned(
                buf.as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|pixel| {
                        if pixel[3] == 0 {
                            [0, 0, 0, 0]
                        } else {
                            [
                                ((u32::from(pixel[0]) * 255 + u32::from(pixel[3]) / 2)
                                    / u32::from(pixel[3]))
                                .min(255) as u8,
                                ((u32::from(pixel[1]) * 255 + u32::from(pixel[3]) / 2)
                                    / u32::from(pixel[3]))
                                .min(255) as u8,
                                ((u32::from(pixel[2]) * 255 + u32::from(pixel[3]) / 2)
                                    / u32::from(pixel[3]))
                                .min(255) as u8,
                                pixel[3],
                            ]
                        }
                    })
                    .collect(),
            ),
        };
        let data = match &spec.encoding {
            Encoding::Png(compression) => {
                let mut out = Vec::new();
                let mut encoder = png::Encoder::new(Cursor::new(&mut out), dev_w, dev_h);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                match compression {
                    PngCompression::Fast => encoder.set_compression(png::Compression::Fast),
                    PngCompression::Default => {}
                    PngCompression::Best => encoder.set_compression(png::Compression::High),
                }
                let mut writer = encoder
                    .write_header()
                    .map_err(|error| CaptureError::Render(error.to_string()))?;
                writer
                    .write_image_data(&pixels)
                    .map_err(|error| CaptureError::Render(error.to_string()))?;
                writer
                    .finish()
                    .map_err(|error| CaptureError::Render(error.to_string()))?;
                out
            }
            Encoding::Jpeg { quality } => {
                let (Ok(width), Ok(height)) = (u16::try_from(dev_w), u16::try_from(dev_h)) else {
                    return Err(CaptureError::Render("jpeg dimensions exceed 65535".into()));
                };
                let mut out = Vec::new();
                jpeg_encoder::Encoder::new(&mut out, *quality)
                    .encode(&pixels, width, height, jpeg_encoder::ColorType::Rgba)
                    .map_err(|error| CaptureError::Render(error.to_string()))?;
                out
            }
            Encoding::WebP { quality } => {
                let encoder = webp::Encoder::from_rgba(&pixels, dev_w, dev_h);
                quality.as_ref().map_or_else(
                    || encoder.encode_lossless().to_vec(),
                    |quality| encoder.encode(f32::from(*quality)).to_vec(),
                )
            }
            Encoding::Qoi => qoi::encode_to_vec(&pixels, dev_w, dev_h)
                .map_err(|error| CaptureError::Render(error.to_string()))?,
        };
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

fn scaled_pixels(length: f32, pixel_ratio: f32) -> u32 {
    let scaled = (length * pixel_ratio).ceil().max(1.0);
    u32::try_from(scaled as i64).unwrap_or(u32::MAX)
}

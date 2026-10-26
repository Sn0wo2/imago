use std::borrow::Cow;
use std::io::Cursor;

use peniko::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    WebP,
    Qoi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PngCompression {
    Fast,
    Default,
    Best,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Png(PngCompression),
    Jpeg { quality: u8 },
    WebP { quality: Option<u8> },
    Qoi,
}

pub(crate) fn encode(
    premul: &[u8],
    w: u32,
    h: u32,
    encoding: &Encoding,
    background: Option<Color>,
) -> Result<Vec<u8>, String> {
    let pixels = match (
        encoding,
        premul.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
    ) {
        (_, true) => Cow::Borrowed(premul),
        (Encoding::Jpeg { .. }, false) => {
            let [br, bg, bb, _] = background
                .unwrap_or(Color::new([1.0, 1.0, 1.0, 1.0]))
                .components
                .map(|channel| (channel * 255.0).round() as u32);
            Cow::Owned(
                premul
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|px| {
                        [
                            (px[0] as u32 + br * (255 - px[3] as u32) / 255).min(255) as u8,
                            (px[1] as u32 + bg * (255 - px[3] as u32) / 255).min(255) as u8,
                            (px[2] as u32 + bb * (255 - px[3] as u32) / 255).min(255) as u8,
                            255,
                        ]
                    })
                    .collect(),
            )
        }
        (_, false) => Cow::Owned(
            premul
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| {
                    if px[3] == 0 {
                        [0, 0, 0, 0]
                    } else {
                        [
                            ((px[0] as u32 * 255 + px[3] as u32 / 2) / px[3] as u32).min(255) as u8,
                            ((px[1] as u32 * 255 + px[3] as u32 / 2) / px[3] as u32).min(255) as u8,
                            ((px[2] as u32 * 255 + px[3] as u32 / 2) / px[3] as u32).min(255) as u8,
                            px[3],
                        ]
                    }
                })
                .collect(),
        ),
    };
    match encoding {
        Encoding::Png(compression) => {
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(Cursor::new(&mut out), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            match compression {
                PngCompression::Fast => enc.set_compression(png::Compression::Fast),
                PngCompression::Default => {}
                PngCompression::Best => enc.set_compression(png::Compression::High),
            }
            let mut writer = enc.write_header().expect("png write_header");
            writer
                .write_image_data(&pixels)
                .expect("png write_image_data");
            writer.finish().expect("png finish");
            Ok(out)
        }
        Encoding::Jpeg { quality } => {
            let (w, h) = match (u16::try_from(w), u16::try_from(h)) {
                (Ok(w), Ok(h)) => (w, h),
                _ => return Err("jpeg dimensions exceed 65535".into()),
            };
            let mut out = Vec::new();
            jpeg_encoder::Encoder::new(&mut out, *quality)
                .encode(&pixels, w, h, jpeg_encoder::ColorType::Rgba)
                .map_err(|e| e.to_string())?;
            Ok(out)
        }
        Encoding::WebP { quality } => {
            let enc = webp::Encoder::from_rgba(&pixels, w, h);
            Ok(match quality {
                Some(q) => enc.encode(*q as f32).to_vec(),
                None => enc.encode_lossless().to_vec(),
            })
        }
        Encoding::Qoi => qoi::encode_to_vec(&pixels, w, h).map_err(|e| e.to_string()),
    }
}

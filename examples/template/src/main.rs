use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use imago::template::{context, Environment};
use imago::{CaptureOptions, CaptureSize, Imago};
use serde_json::Value;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut data_path = None;
    let mut web_dir = manifest_dir.join("web");
    let mut output_dir = manifest_dir;
    let mut pixel_ratio = 1.5_f32;
    let mut width = None;
    let mut max_height = 16_000;
    let mut workers = 1;
    let mut timeout = 30;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: cargo run --manifest-path examples/template/Cargo.toml -- [data.json] [--data PATH] [--web-dir DIR] [--output-dir DIR] [--pixel-ratio N] [--width PX] [--max-height PX] [--workers N] [--timeout SECONDS]"
                );
                return Ok(());
            }
            "--data" => {
                if data_path.is_some() {
                    return Err("data path specified more than once".into());
                }
                data_path = Some(PathBuf::from(args.next().ok_or("--data requires a path")?));
            }
            "--web-dir" => {
                web_dir = PathBuf::from(args.next().ok_or("--web-dir requires a path")?);
            }
            "--output-dir" => {
                output_dir = PathBuf::from(args.next().ok_or("--output-dir requires a path")?);
            }
            "--pixel-ratio" => {
                pixel_ratio = args
                    .next()
                    .ok_or("--pixel-ratio requires a value")?
                    .parse()?;
            }
            "--width" => {
                width = Some(args.next().ok_or("--width requires a value")?.parse()?);
            }
            "--max-height" => {
                max_height = args
                    .next()
                    .ok_or("--max-height requires a value")?
                    .parse()?;
            }
            "--workers" => {
                workers = args.next().ok_or("--workers requires a value")?.parse()?;
            }
            "--timeout" => {
                timeout = args.next().ok_or("--timeout requires a value")?.parse()?;
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}").into()),
            _ if data_path.is_some() => return Err("expected only one data path".into()),
            _ => data_path = Some(PathBuf::from(arg)),
        }
    }

    if !pixel_ratio.is_finite() || pixel_ratio <= 0.0 {
        return Err("pixel ratio must be a positive finite number".into());
    }
    if max_height == 0 || workers == 0 || timeout == 0 {
        return Err("max height, workers, and timeout must be positive".into());
    }

    let data_path =
        data_path.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data.json"));
    let data = serde_json::from_str::<Value>(&std::fs::read_to_string(data_path)?)?;
    let columns = data.get("columns").and_then(Value::as_u64).unwrap_or(3);
    if !(1..=10).contains(&columns) {
        return Err("columns must be between 1 and 10".into());
    }
    if !data
        .get("cards")
        .and_then(Value::as_array)
        .is_some_and(|cards| !cards.is_empty())
    {
        return Err("cards must be a non-empty array".into());
    }
    let page_width = width.unwrap_or((columns as u32 * 290 + 64).max(620));
    if page_width == 0 {
        return Err("width must be positive".into());
    }

    let mut environment = Environment::new();
    environment.set_undefined_behavior(imago::template::UndefinedBehavior::Strict);
    let style = std::fs::read_to_string(web_dir.join("style.css"))?;
    let page = std::fs::read_to_string(web_dir.join("page.html"))?;
    environment.add_template("style.css", &style)?;
    environment.add_template("page.html", &page)?;
    let mut spec = CaptureOptions::from_template(
        &environment.get_template("page.html")?,
        context! {
            data => imago::template::value::Serde(data),
            columns,
            page_width,
        },
    )?;
    spec.base_url = Some(format!("{}/", imago::utils::file_url(&web_dir)));
    spec.size = CaptureSize::FullPage {
        width: page_width,
        max_height: Some(max_height),
    };
    spec.pixel_ratio = pixel_ratio;
    spec.timeout = Duration::from_secs(timeout);
    std::fs::create_dir_all(&output_dir)?;
    std::fs::write(output_dir.join("output.html"), &spec.html)?;
    let image = Imago::spawn(workers).submit(spec).result().await?;
    std::fs::write(output_dir.join("output.png"), image.data)?;
    println!(
        "Saved output.html and output.png ({}×{})",
        image.width, image.height
    );
    Ok(())
}

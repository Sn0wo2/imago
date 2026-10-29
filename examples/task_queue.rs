use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use imago::{CaptureOptions, CaptureSize, Imago, Priority};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let imago = Imago::spawn(1);
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let task_template = std::fs::read_to_string(examples_dir.join("task_queue.html"))?;
    let base_url = format!("{}/", imago::utils::file_url(&examples_dir));

    let mut specs = ["URGENT", "n1", "n2", "n3", "n4"]
        .map(|tag| {
            let mut spec = CaptureOptions::new(task_template.replace("TASK_TAG", tag));
            spec.size = CaptureSize::Viewport {
                width: 1024,
                height: 1880,
            };
            spec.pixel_ratio = 1.0;
            spec.base_url = Some(base_url.clone());
            (tag, spec)
        })
        .into_iter();
    let (_, mut urgent_spec) = specs.next().ok_or("no task specifications")?;
    urgent_spec.priority = Priority::Urgent;
    let mut tickets = specs
        .map(|(tag, spec)| (tag, imago.submit(spec)))
        .collect::<Vec<_>>();

    let urgent = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        imago.submit(urgent_spec)
    })
    .await?;
    tickets.push(("URGENT", urgent));

    let done = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for (tag, ticket) in tickets {
        let done = Arc::clone(&done);
        tasks.push(tokio::spawn(async move {
            let output = ticket.result().await.map_err(std::io::Error::other)?;
            let filename = format!("output_{}.png", tag.replace(':', "_"));
            std::fs::write(&filename, output.data)?;
            writeln!(
                std::io::stdout(),
                "Saved {filename} ({}x{})",
                output.width,
                output.height
            )?;
            done.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((t0.elapsed().as_secs_f64() * 1000.0, tag));
            Ok::<_, std::io::Error>(())
        }));
    }
    for t in tasks {
        t.await??;
    }

    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "Submission order: n1, n2, n3, n4 (URGENT is submitted after 30 ms)"
    )?;
    writeln!(stdout, "Completion order:")?;
    for (ms, tag) in done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
    {
        writeln!(stdout, "  {ms:8.1}ms  {tag}")?;
    }
    Ok(())
}

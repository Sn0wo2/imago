use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use imago::{CaptureOptions, CaptureSize, Imago, Priority};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let t0 = Instant::now();
    let imago = Imago::spawn(1);
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let task_template = std::fs::read_to_string(examples_dir.join("task_queue.html"))
        .expect("read task queue HTML");
    let base_url = format!("{}/", imago::utils::file_url(&examples_dir));

    let mut specs = ["URGENT", "n1", "n2", "n3", "n4"]
        .map(|tag| {
            (
                tag,
                CaptureOptions {
                    size: CaptureSize::Viewport {
                        width: 1024,
                        height: 1880,
                    },
                    pixel_ratio: 1.0,
                    base_url: Some(base_url.clone()),
                    ..CaptureOptions::new(task_template.replace("TASK_TAG", tag))
                },
            )
        })
        .into_iter();
    let (_, mut urgent_spec) = specs.next().unwrap();
    urgent_spec.priority = Priority::Urgent;
    let mut tickets = specs
        .map(|(tag, spec)| (tag, imago.submit(spec)))
        .collect::<Vec<_>>();

    let urgent = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        imago.submit(urgent_spec)
    })
    .await
    .expect("join");
    tickets.push(("URGENT", urgent));

    let done = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for (tag, ticket) in tickets {
        let done = Arc::clone(&done);
        tasks.push(tokio::spawn(async move {
            let output = ticket.result().await.expect("capture");
            let filename = format!("output_{}.png", tag.replace(':', "_"));
            std::fs::write(&filename, output.data).expect("write PNG");
            println!("Saved {filename} ({}x{})", output.width, output.height);
            done.lock()
                .unwrap()
                .push((t0.elapsed().as_secs_f64() * 1000.0, tag));
        }));
    }
    for t in tasks {
        t.await.expect("task");
    }

    println!("Submission order: n1, n2, n3, n4 (URGENT is submitted after 30 ms)");
    println!("Completion order:");
    for (ms, tag) in done.lock().unwrap().iter() {
        println!("  {ms:8.1}ms  {tag}");
    }
}

use lindiction::audio::start_capture;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let (_stream, mut rx) = start_capture(16_000, 1)?;
    let start = Instant::now();
    let mut chunks = 0usize;
    let mut samples = 0usize;

    while start.elapsed() < Duration::from_secs(5) {
        if let Some(chunk) = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .ok()
            .flatten()
        {
            chunks += 1;
            samples += chunk.len();
            let rms = (chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len() as f32).sqrt();
            println!("chunk: {:4} samples  rms: {:.4}", chunk.len(), rms);
        }
    }

    println!(
        "captured {} chunks / {} samples in 5s (~{} Hz effective)",
        chunks,
        samples,
        samples / 5
    );
    Ok(())
}

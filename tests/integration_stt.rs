use lindiction::stt::SttEngine;
use std::path::PathBuf;

fn load_wav(path: &str) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("open fixture");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "fixture must be 16 kHz");
    assert_eq!(spec.channels, 1, "fixture must be mono");
    match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.expect("sample") as f32 / i16::MAX as f32)
            .collect(),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("sample"))
            .collect(),
    }
}

#[test]
fn transcribes_hello_world_fixture() {
    // Gate on env var so `cargo test` works in environments without a model file.
    let model = match std::env::var("LINDICTION_MODEL") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("skipping: set LINDICTION_MODEL to run this test");
            return;
        }
    };
    if !model.exists() {
        eprintln!("skipping: {} does not exist", model.display());
        return;
    }

    let engine = SttEngine::load(&model).expect("load model");
    let audio = load_wav("tests/fixtures/hello.wav");
    let text = engine.transcribe(&audio).expect("transcribe");
    let lc = text.to_lowercase();
    assert!(
        lc.contains("hello"),
        "expected transcript to contain 'hello', got: {text:?}"
    );
}

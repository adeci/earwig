//! transcribe.cpp on Metal and on the CPU: load a model, decode a clip.

use std::path::Path;
use std::time::Instant;

use transcribe_cpp as tc;

pub fn run(model: &Path, wav: &Path, backend: &str) -> anyhow::Result<()> {
    tc::init_logging();
    let request = match backend {
        "metal" => tc::Backend::Metal,
        "cpu" => tc::Backend::Cpu,
        _ => tc::Backend::Auto,
    };
    crate::log(&format!(
        "transcribe: metal available={} cpu available={}",
        tc::backend_available(tc::Backend::Metal),
        tc::backend_available(tc::Backend::Cpu)
    ));
    let mut reader = hound::WavReader::open(wav)?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate == 16_000 && spec.channels == 1,
        "need 16 kHz mono, got {spec:?}"
    );
    let pcm: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|s| f32::from(s) / 32768.))
        .collect::<Result<_, _>>()?;
    let started = Instant::now();
    let model = tc::Model::load_with(
        model,
        &tc::ModelOptions {
            backend: request,
            ..tc::ModelOptions::default()
        },
    )?;
    let device = model
        .device()
        .map_or_else(|e| format!("({e})"), |d| d.kind.clone());
    crate::log(&format!(
        "transcribe: {backend}: loaded {} on {} / {device} in {:?}",
        model.arch(),
        model.backend(),
        started.elapsed()
    ));
    let mut session = model.session()?;
    for pass in 1..=3 {
        let started = Instant::now();
        let text = session.run(&pcm, &tc::RunOptions::default())?.text;
        crate::log(&format!(
            "transcribe: {backend} pass {pass}: {:?} in {:?} ({:.1} s of audio)",
            text.trim(),
            started.elapsed(),
            pcm.len() as f32 / 16_000.
        ));
    }
    Ok(())
}

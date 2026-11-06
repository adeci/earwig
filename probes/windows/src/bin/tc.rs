//! tc <model.gguf> <audio.wav> [vulkan|cpu]: device list, then one transcription.
use std::time::Instant;

use transcribe_cpp::{Backend, Model, ModelOptions, RunOptions};

fn main() -> anyhow::Result<()> {
    transcribe_cpp::init_backends_default()?;
    for d in transcribe_cpp::devices() {
        println!("device {} [{}] {}", d.name, d.kind, d.description);
    }
    for b in [Backend::Cpu, Backend::Vulkan, Backend::Cuda] {
        println!("{b:?} available: {}", transcribe_cpp::backend_available(b));
    }
    let mut args = std::env::args().skip(1);
    let (Some(model), Some(audio)) = (args.next(), args.next()) else {
        return Ok(());
    };
    let backend = match args.next().as_deref() {
        Some("cpu") => Backend::Cpu,
        _ => Backend::Vulkan,
    };
    let reader = hound::WavReader::open(&audio)?;
    let spec = reader.spec();
    println!("wav {spec:?}");
    let pcm: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.)
            .collect(),
        hound::SampleFormat::Float => reader.into_samples::<f32>().map(Result::unwrap).collect(),
    };
    let t = Instant::now();
    let model = Model::load_with(
        &model,
        &ModelOptions {
            backend,
            ..Default::default()
        },
    )?;
    println!("loaded {} on {} in {:?}", model.arch(), model.backend(), t.elapsed());
    let mut session = model.session()?;
    let t = Instant::now();
    let result = session.run(&pcm, &RunOptions::default())?;
    println!("ran in {:?}: {}", t.elapsed(), result.text.trim());
    Ok(())
}

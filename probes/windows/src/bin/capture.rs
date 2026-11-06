//! capture [seconds]: list input devices, record from the default one, print levels.
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let host = cpal::default_host();
    println!("host {:?}", host.id());
    for device in host.input_devices()? {
        println!(
            "input {:?} id={:?}",
            device.description().map(|d| d.name().to_owned()),
            device.id()
        );
    }
    let device = host.default_input_device().expect("no default input");
    let config = device.default_input_config()?;
    println!(
        "default {:?} {config:?}",
        device.description().map(|d| d.name().to_owned())
    );
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = samples.clone();
    let channels = config.channels() as usize;
    let stream = device.build_input_stream(
        config.config(),
        move |data: &[f32], _| {
            sink.lock()
                .unwrap()
                .extend(data.chunks(channels).map(|f| f[0]));
        },
        |e| eprintln!("stream error {e}"),
        None,
    )?;
    stream.play()?;
    std::thread::sleep(std::time::Duration::from_secs(secs));
    drop(stream);
    let samples = samples.lock().unwrap();
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    let peak = samples.iter().fold(0f32, |a, s| a.max(s.abs()));
    println!("{} frames, rms {rms:.5}, peak {peak:.4}", samples.len());
    Ok(())
}

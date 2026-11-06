//! Record from the default input for a few seconds and report the levels.

use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

pub fn run(secs: u64) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let inputs = host.input_devices()?.count();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device"))?;
    let id = device.id().map(|id| id.id().to_owned()).unwrap_or_default();
    let supported = device.default_input_config()?;
    crate::log(&format!(
        "mic: {inputs} input(s); default {id:?}: {} Hz, {} ch, {:?}; microphone permission={}",
        supported.sample_rate(),
        supported.channels(),
        supported.sample_format(),
        crate::perms::microphone()
    ));
    let (tx, rx) = flume::unbounded::<Vec<f32>>();
    let err = |e: cpal::Error| crate::log(&format!("mic: stream error {e}"));
    let config = supported.config();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            config,
            move |data: &[f32], _| {
                let _ = tx.send(data.to_vec());
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
            config,
            move |data: &[i16], _| {
                let _ = tx.send(data.iter().map(|&s| f32::from(s) / 32768.).collect());
            },
            err,
            None,
        )?,
        other => anyhow::bail!("unsupported sample format {other}"),
    };
    let started = Instant::now();
    stream.play()?;
    let mut first = None;
    let (mut n, mut sum, mut peak) = (0usize, 0f64, 0f32);
    let deadline = started + Duration::from_secs(secs);
    while let Ok(chunk) = rx.recv_deadline(deadline) {
        first.get_or_insert_with(|| started.elapsed());
        for s in chunk {
            n += 1;
            sum += f64::from(s) * f64::from(s);
            peak = peak.max(s.abs());
        }
    }
    drop(stream);
    #[allow(clippy::cast_precision_loss)]
    let rms = (sum / n.max(1) as f64).sqrt();
    crate::log(&format!(
        "mic: {n} samples in {secs} s, first data after {first:?}, rms {rms:.4}, peak {peak:.4}"
    ));
    Ok(())
}

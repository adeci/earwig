//! macOS: Core Audio through cpal. `[audio] device` holds a device's UID,
//! the same across runs; it is shown by the name the Sound settings use.

use cpal::traits::DeviceTrait as _;

use super::{InputDevice, InputDevices, Opened, Raw};

pub(super) type Stream = cpal::Stream;

/// `configured` adds nothing: every device that opens is listed.
pub(super) fn input_devices(_configured: Option<&str>) -> anyhow::Result<InputDevices> {
    super::cpal_devices(|device| {
        let id = uid(device)?;
        let name = device
            .description()
            .map_or_else(|_| id.clone(), |d| d.name().to_owned());
        Some(InputDevice { id, name })
    })
}

pub(super) fn open(device: Option<&str>, tx: flume::Sender<Raw>) -> anyhow::Result<Opened> {
    // Without Microphone access macOS hands out silence, not an error.
    if crate::macos::microphone_allowed() == Some(false) {
        anyhow::bail!(crate::permissions::missing_message(
            crate::permissions::Permission::Microphone
        ));
    }
    let (device, id) = super::cpal_input(device, uid)?;
    let (stream, rate) = super::open_cpal(&device, &id, tx)?;
    Ok(Opened {
        stream,
        rate,
        name: id,
    })
}

fn uid(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_owned())
}

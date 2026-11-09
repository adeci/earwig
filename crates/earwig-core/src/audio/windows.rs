//! Windows: WASAPI through cpal. A device goes by the name the Sound
//! settings show, in `[audio] device` too: its id is an opaque GUID.

use cpal::traits::DeviceTrait as _;

use super::{InputDevice, InputDevices, Opened, Raw};

pub(super) type Stream = cpal::Stream;

/// `configured` adds nothing: every device that opens is listed.
pub(super) fn input_devices(_configured: Option<&str>) -> anyhow::Result<InputDevices> {
    super::cpal_devices(|device| {
        friendly_name(device).map(|name| InputDevice {
            id: name.clone(),
            name,
        })
    })
}

pub(super) fn open(device: Option<&str>, tx: flume::Sender<Raw>) -> anyhow::Result<Opened> {
    let (device, name) = super::cpal_input(device, friendly_name)?;
    let (stream, rate) = super::open_cpal(&device, &name, tx)?;
    Ok(Opened { stream, rate, name })
}

fn friendly_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

//! The sound server over the PulseAudio protocol, which PipeWire's
//! pipewire-pulse speaks too: its sources, and recording from one. Plain
//! blocking calls on its socket, with timeouts, so a hung server costs an
//! error rather than a stuck thread.

use std::ffi::{CStr, CString};
use std::io::{self, BufReader, Read as _};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context as _;
use pulseaudio::protocol::{self, Command, ProtocolError, PulseError};

use crate::audio::{InputDevice, InputDevices, Raw};

/// How long the server may take to answer.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Audio arrives in 20 ms fragments, like a capture period: the overlay
/// follows the voice, and the server need not wake earwig more often.
const FRAGMENTS_PER_SECOND: u32 = 50;
/// Bytes in a frame of the recording: one `f32`.
const FRAME_BYTES: u32 = 4;
/// No message earwig gets comes near this; a longer one means a broken
/// connection, not worth the memory.
const MAX_MESSAGE: usize = 16 << 20;
/// The length of the cookie a client authenticates with.
const COOKIE_LENGTH: usize = 256;

/// A connection to the sound server.
pub(super) struct Server {
    socket: BufReader<UnixStream>,
    version: u16,
    seq: u32,
    /// The last message read, header and payload.
    message: Vec<u8>,
}

impl Server {
    /// The sound server, or `None` when none runs.
    pub(super) fn connect() -> anyhow::Result<Option<Self>> {
        let Some(path) = pulseaudio::socket_path_from_env() else {
            return Ok(None);
        };
        let socket = match UnixStream::connect(&path) {
            Ok(socket) => socket,
            // Left behind by a server that is gone.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                return Ok(None);
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("connecting to the sound server at {}", path.display())
                });
            }
        };
        socket.set_read_timeout(Some(TIMEOUT))?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        let mut server = Self {
            socket: BufReader::new(socket),
            version: protocol::MAX_VERSION,
            seq: 0,
            message: Vec::new(),
        };
        server
            .introduce()
            .context("connecting to the sound server")?;
        Ok(Some(server))
    }

    fn introduce(&mut self) -> Result<(), ProtocolError> {
        // PulseAudio checks the cookie it shares with its clients;
        // pipewire-pulse only wants one of the right length, and there may
        // be no cookie file for it.
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_else(|| vec![0; COOKIE_LENGTH]);
        let auth: protocol::AuthReply = self.roundtrip(&Command::Auth(protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }))?;
        self.version = auth.version.min(protocol::MAX_VERSION);
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, c"earwig");
        let _: protocol::SetClientNameReply = self.roundtrip(&Command::SetClientName(props))?;
        Ok(())
    }

    /// Its microphones and its default source.
    pub(super) fn microphones(&mut self) -> anyhow::Result<InputDevices> {
        let sources: protocol::SourceInfoList = self
            .roundtrip(&Command::GetSourceInfoList)
            .context("listing the sound server's sources")?;
        let server: protocol::ServerInfo = self
            .roundtrip(&Command::GetServerInfo)
            .context("asking the sound server for its default source")?;
        Ok(microphones(&sources, server.default_source_name.as_deref()))
    }

    /// The source called `name`, if there is one.
    pub(super) fn source(&mut self, name: &str) -> anyhow::Result<Option<protocol::SourceInfo>> {
        let Ok(name) = CString::new(name) else {
            return Ok(None);
        };
        let query = protocol::GetSourceInfo {
            index: None,
            name: Some(name),
        };
        match self.roundtrip(&Command::GetSourceInfo(query)) {
            Ok(source) => Ok(Some(source)),
            Err(ProtocolError::ServerError(PulseError::NoEntity)) => Ok(None),
            Err(e) => Err(e).context("asking the sound server for a source"),
        }
    }

    /// Record `source` as mono `f32` at its own rate into `tx` until the
    /// [`Recording`] is dropped; returns it and the rate. If the source goes
    /// away, so does the recording, rather than moving to another one.
    pub(super) fn record(
        mut self,
        source: &protocol::SourceInfo,
        tx: flume::Sender<Raw>,
    ) -> anyhow::Result<(Recording, u32)> {
        let name = source.name.to_string_lossy().into_owned();
        let rate = source.sample_spec.sample_rate;
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::MediaName, c"Dictation");
        let params = protocol::RecordStreamParams {
            sample_spec: protocol::SampleSpec {
                format: protocol::SampleFormat::Float32Le,
                channels: 1,
                sample_rate: rate,
            },
            channel_map: protocol::ChannelMap::mono(),
            source_index: Some(source.index),
            buffer_attr: protocol::stream::BufferAttr {
                fragment_size: rate / FRAGMENTS_PER_SECOND * FRAME_BYTES,
                ..Default::default()
            },
            flags: protocol::stream::StreamFlags {
                // The source itself works in fragments that size.
                adjust_latency: true,
                no_move: true,
                ..Default::default()
            },
            props,
            ..Default::default()
        };
        let stream: protocol::CreateRecordStreamReply = self
            .roundtrip(&Command::CreateRecordStream(params))
            .with_context(|| format!("recording from {name}"))?;
        // From here on the server sends audio, at its own pace.
        self.socket.get_ref().set_read_timeout(None)?;
        let socket = self.socket.get_ref().try_clone()?;
        let channel = stream.channel;
        let reader = thread::Builder::new()
            .name("earwig-pulse".into())
            .spawn(move || self.forward(channel, &tx))?;
        let rate = stream.sample_spec.sample_rate;
        let fragment = stream.buffer_attr.fragment_size / FRAME_BYTES;
        log::info!(
            "audio input {name}: {rate} Hz, mono from the sound server, \
             fragments of {fragment} frames"
        );
        Ok((
            Recording {
                socket,
                reader: Some(reader),
            },
            rate,
        ))
    }

    /// Hand the audio of stream `channel` to `tx` until the connection or
    /// the stream ends.
    fn forward(mut self, channel: u32, tx: &flume::Sender<Raw>) {
        let error = loop {
            let descriptor = match self.read_message() {
                Ok(descriptor) => descriptor,
                Err(e) => break format!("sound server: {e}"),
            };
            let payload = &self.message[protocol::DESCRIPTOR_SIZE..];
            if descriptor.channel == channel {
                let (frames, _) = payload.as_chunks::<4>();
                let pcm = frames.iter().map(|&frame| f32::from_le_bytes(frame));
                if tx.send(Raw::Data(pcm.collect())).is_err() {
                    return;
                }
            } else if descriptor.channel == u32::MAX
                && let Ok((_, Command::RecordStreamKilled(killed))) =
                    Command::read_tag_prefixed(&mut &payload[..], self.version)
                && killed == channel
            {
                break "the sound server ended the recording".to_owned();
            }
        };
        let _ = tx.send(Raw::Error(error));
    }

    fn roundtrip<R: protocol::CommandReply>(
        &mut self,
        command: &Command,
    ) -> Result<R, ProtocolError> {
        let seq = self.seq;
        self.seq += 1;
        protocol::write_command_message(self.socket.get_mut(), seq, command, self.version)?;
        self.read_message()?;
        let (_, reply) = protocol::read_reply_message(&mut &self.message[..], self.version)?;
        Ok(reply)
    }

    /// Read the next message, whole, into `message`; returns its header.
    fn read_message(&mut self) -> Result<protocol::Descriptor, ProtocolError> {
        self.message.resize(protocol::DESCRIPTOR_SIZE, 0);
        self.socket.read_exact(&mut self.message)?;
        let descriptor = protocol::read_descriptor(&mut &self.message[..])?;
        let length = protocol::DESCRIPTOR_SIZE + descriptor.length as usize;
        if length > MAX_MESSAGE {
            return Err(ProtocolError::Invalid(format!(
                "a message of {length} bytes"
            )));
        }
        self.message.resize(length, 0);
        self.socket
            .read_exact(&mut self.message[protocol::DESCRIPTOR_SIZE..])?;
        Ok(descriptor)
    }
}

/// A recording from the sound server, until dropped.
pub(in crate::audio) struct Recording {
    /// The connection the stream lives on; shutting it ends the stream and
    /// wakes the reader.
    socket: UnixStream,
    reader: Option<JoinHandle<()>>,
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The microphones among the server's `sources`, by the descriptions the
/// desktop's sound settings show: every source but the monitors of outputs.
/// The default is named even when it is a monitor.
fn microphones(sources: &[protocol::SourceInfo], default: Option<&CStr>) -> InputDevices {
    let device = |source: &protocol::SourceInfo| {
        let id = source.name.to_string_lossy().into_owned();
        let name = source
            .description
            .as_deref()
            .map_or_else(|| id.clone(), |name| name.to_string_lossy().into_owned());
        InputDevice { id, name }
    };
    InputDevices {
        devices: sources
            .iter()
            .filter(|source| source.monitor_of_sink_index.is_none())
            .map(device)
            .collect(),
        default: default
            .and_then(|name| sources.iter().find(|source| source.name.as_c_str() == name))
            .map(device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(index: u32, name: &CStr, description: Option<&CStr>) -> protocol::SourceInfo {
        protocol::SourceInfo {
            index,
            name: name.into(),
            description: description.map(Into::into),
            ..Default::default()
        }
    }

    fn monitor(index: u32, name: &CStr, description: &CStr, sink: u32) -> protocol::SourceInfo {
        protocol::SourceInfo {
            monitor_of_sink_index: Some(sink),
            ..source(index, name, Some(description))
        }
    }

    fn device(id: &str, name: &str) -> InputDevice {
        InputDevice {
            id: id.into(),
            name: name.into(),
        }
    }

    /// As a laptop's PipeWire lists them: speakers, a USB microphone, and a
    /// loopback whose input side is an output with a monitor of its own.
    fn laptop() -> Vec<protocol::SourceInfo> {
        vec![
            monitor(
                41,
                c"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
                c"Monitor of Built-in Audio Analog Stereo",
                40,
            ),
            source(
                42,
                c"alsa_input.pci-0000_00_1f.3.analog-stereo",
                Some(c"Built-in Audio Analog Stereo"),
            ),
            source(
                57,
                c"alsa_input.usb-Shure_Inc_Shure_MV7-00.mono-fallback",
                Some(c"Shure MV7 Mono"),
            ),
            monitor(
                61,
                c"effect_input.rnnoise.monitor",
                c"Monitor of Noise Canceling source",
                60,
            ),
            source(
                62,
                c"effect_output.rnnoise",
                Some(c"Noise Canceling source"),
            ),
            source(70, c"virtual_mic", None),
        ]
    }

    #[test]
    fn microphones_are_the_sources_but_monitors_by_description() {
        let found = microphones(
            &laptop(),
            Some(c"alsa_input.usb-Shure_Inc_Shure_MV7-00.mono-fallback"),
        );
        assert_eq!(
            found.devices,
            [
                device(
                    "alsa_input.pci-0000_00_1f.3.analog-stereo",
                    "Built-in Audio Analog Stereo"
                ),
                device(
                    "alsa_input.usb-Shure_Inc_Shure_MV7-00.mono-fallback",
                    "Shure MV7 Mono"
                ),
                device("effect_output.rnnoise", "Noise Canceling source"),
                // Without a description, by its name.
                device("virtual_mic", "virtual_mic"),
            ]
        );
        assert_eq!(
            found.default,
            Some(device(
                "alsa_input.usb-Shure_Inc_Shure_MV7-00.mono-fallback",
                "Shure MV7 Mono"
            ))
        );
    }

    #[test]
    fn the_default_is_named_even_when_it_is_a_monitor_or_unknown_when_gone() {
        let sources = laptop();
        let monitor = c"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor";
        assert_eq!(
            microphones(&sources, Some(monitor)).default,
            Some(device(
                "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
                "Monitor of Built-in Audio Analog Stereo"
            ))
        );
        assert_eq!(microphones(&sources, Some(c"unplugged")).default, None);
        assert_eq!(microphones(&sources, None).default, None);
        assert_eq!(microphones(&[], None), InputDevices::default());
    }
}

//! Linux: the microphones are the sound server's sources (PipeWire through
//! pipewire-pulse, or PulseAudio) by the descriptions the desktop's sound
//! settings show, monitors of outputs left out. Both servers speak the
//! PulseAudio protocol ([`pulse`]), and a chosen source is recorded over it.
//! The system default goes through ALSA's `default`, which on a desktop is
//! the server's own plugin and follows the default as it changes.
//!
//! Without a sound server each sound card that can record is a microphone,
//! opened through ALSA. Settings from before earwig listed microphones name
//! ALSA PCMs (`pipewire`, `sysdefault:CARD=MV7`, `hw:CARD=1,DEV=0`); such a
//! name still opens through ALSA.

mod pulse;

use std::fs;
use std::path::Path;

use cpal::traits::DeviceTrait as _;

use super::{InputDevice, InputDevices, Opened, Raw};

/// The kernel's view of the sound cards.
const ASOUND: &str = "/proc/asound";

/// Keeps an input open until dropped.
#[expect(dead_code, reason = "a stream is held only to be dropped")]
pub(super) enum Stream {
    Alsa(cpal::Stream),
    Server(pulse::Recording),
}

pub(super) fn input_devices(configured: Option<&str>) -> anyhow::Result<InputDevices> {
    let mut found = match pulse::Server::connect()? {
        Some(mut server) => server.microphones()?,
        None => cards(Path::new(ASOUND)),
    };
    keep_configured(&mut found, configured, |name| {
        super::cpal_input(Some(name), alsa_id).is_ok()
    });
    Ok(found)
}

pub(super) fn open(device: Option<&str>, tx: flume::Sender<Raw>) -> anyhow::Result<Opened> {
    if let Some(name) = device
        && let Some(mut server) = pulse::Server::connect()?
        && let Some(source) = server.source(name)?
    {
        let (recording, rate) = server.record(&source, tx)?;
        return Ok(Opened {
            stream: Stream::Server(recording),
            rate,
            name: name.to_owned(),
        });
    }
    // The system default, a sound card without a server, or an ALSA PCM
    // name from older settings.
    let (device, name) = super::cpal_input(device, alsa_id)?;
    let (stream, rate) = super::open_cpal(&device, &name, tx)?;
    Ok(Opened {
        stream: Stream::Alsa(stream),
        rate,
        name,
    })
}

/// An ALSA PCM name, such as `default` or `sysdefault:CARD=MV7`.
fn alsa_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_owned())
}

/// Lists `configured` under its own name when it is not listed but `opens`:
/// an ALSA PCM name from settings made before earwig listed microphones.
fn keep_configured(
    found: &mut InputDevices,
    configured: Option<&str>,
    opens: impl FnOnce(&str) -> bool,
) {
    if let Some(configured) = configured
        && !found.devices.iter().any(|d| d.id == configured)
        && opens(configured)
    {
        found.devices.push(InputDevice {
            id: configured.to_owned(),
            name: configured.to_owned(),
        });
    }
}

/// One microphone per sound card that records, for systems without a sound
/// server, named as the kernel names the card. It opens through the card's
/// `sysdefault` PCM, which converts formats and lets other apps record too.
fn cards(asound: &Path) -> InputDevices {
    let Ok(list) = fs::read_to_string(asound.join("cards")) else {
        return InputDevices::default();
    };
    let cards: Vec<Card> = parse_cards(&list)
        .into_iter()
        .filter(|card| records(&asound.join(format!("card{}", card.index))))
        .collect();
    InputDevices {
        devices: cards.iter().map(Card::device).collect(),
        // ALSA's stock configuration records from card 0.
        default: cards.iter().find(|card| card.index == 0).map(Card::device),
    }
}

struct Card {
    index: u32,
    /// The kernel's id for it, such as `MV7`: unlike the index, the same
    /// whatever order the cards come up in.
    id: String,
    name: String,
}

impl Card {
    fn device(&self) -> InputDevice {
        InputDevice {
            id: format!("sysdefault:CARD={}", self.id),
            name: self.name.clone(),
        }
    }
}

/// The cards in `/proc/asound/cards`, two lines each:
///
/// ```text
///  1 [MV7            ]: USB-Audio - Shure MV7
///                       Shure Inc Shure MV7 at usb-0000:00:14.0-2, full speed
/// ```
///
/// Each is named by its long name (the second line) up to where the card
/// sits, else by its short name.
fn parse_cards(list: &str) -> Vec<Card> {
    let mut cards = Vec::new();
    let mut lines = list.lines();
    while let Some(line) = lines.next() {
        let Some((index, rest)) = line.trim_start().split_once(" [") else {
            continue;
        };
        let (Ok(index), Some((id, rest))) = (index.parse(), rest.split_once(']')) else {
            continue;
        };
        let short = rest.split_once(" - ").map_or("", |(_, name)| name.trim());
        let long = lines.next().map_or("", str::trim);
        let long = long.rsplit_once(" at ").map_or(long, |(name, _)| name);
        let name = if long.is_empty() { short } else { long };
        cards.push(Card {
            index,
            id: id.trim().to_owned(),
            name: name.to_owned(),
        });
    }
    cards
}

/// Whether the card whose `/proc/asound/cardN` is `dir` has a capture PCM
/// (`pcm0c`).
fn records(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("pcm") && name.ends_with('c')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0xf7f10000 irq 32
 1 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0xf7080000 irq 17
 2 [MV7            ]: USB-Audio - Shure MV7
                      Shure Inc Shure MV7 at usb-0000:00:14.0-2, full speed
 3 [Headphones     ]: bcm2835_headpho - bcm2835 Headphones
                      bcm2835 Headphones
29 [ThinkPadEC     ]: ThinkPad EC - ThinkPad Console Audio Control
                      ThinkPad Console Audio Control at EC reg 0x30, fw unknown
";

    fn device(id: &str, name: &str) -> InputDevice {
        InputDevice {
            id: id.into(),
            name: name.into(),
        }
    }

    /// A `/proc/asound` with `cards` and these PCMs, e.g. `card0/pcm0c`.
    fn asound(cards: &str, pcms: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("cards"), cards).unwrap();
        for pcm in pcms {
            fs::create_dir_all(dir.path().join(pcm)).unwrap();
        }
        dir
    }

    #[test]
    fn cards_are_named_by_their_long_name_without_where_they_sit() {
        let names: Vec<(u32, String, String)> = parse_cards(CARDS)
            .into_iter()
            .map(|card| (card.index, card.id, card.name))
            .collect();
        assert_eq!(
            names,
            [
                (0, "PCH".into(), "HDA Intel PCH".into()),
                (1, "NVidia".into(), "HDA NVidia".into()),
                (2, "MV7".into(), "Shure Inc Shure MV7".into()),
                (3, "Headphones".into(), "bcm2835 Headphones".into()),
                (
                    29,
                    "ThinkPadEC".into(),
                    "ThinkPad Console Audio Control".into()
                ),
            ]
        );
        assert!(parse_cards("--- no soundcards ---\n").is_empty());
    }

    #[test]
    fn only_cards_that_record_are_microphones() {
        let dir = asound(
            CARDS,
            &[
                "card0/pcm0c",
                "card0/pcm0p",
                "card1/pcm3p",
                "card2/pcm0c",
                "card3/pcm0p",
                "card29",
            ],
        );
        assert_eq!(
            cards(dir.path()),
            InputDevices {
                devices: vec![
                    device("sysdefault:CARD=PCH", "HDA Intel PCH"),
                    device("sysdefault:CARD=MV7", "Shure Inc Shure MV7"),
                ],
                default: Some(device("sysdefault:CARD=PCH", "HDA Intel PCH")),
            }
        );
    }

    #[test]
    fn the_default_is_card_0_when_it_records() {
        let dir = asound(CARDS, &["card1/pcm3p", "card2/pcm0c"]);
        let found = cards(dir.path());
        assert_eq!(
            found.devices,
            [device("sysdefault:CARD=MV7", "Shure Inc Shure MV7")]
        );
        assert_eq!(found.default, None);
    }

    #[test]
    fn no_cards_no_microphones() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cards(dir.path()), InputDevices::default());
        let dir = asound("--- no soundcards ---\n", &[]);
        assert_eq!(cards(dir.path()), InputDevices::default());
    }

    #[test]
    fn an_older_alsa_name_that_still_opens_is_listed_as_itself() {
        let listed = InputDevices {
            devices: vec![device(
                "alsa_input.usb-Shure_Inc_Shure_MV7-00.mono-fallback",
                "Shure MV7 Mono",
            )],
            default: None,
        };
        for legacy in ["pipewire", "sysdefault:CARD=MV7", "hw:CARD=1,DEV=0"] {
            let mut found = listed.clone();
            keep_configured(&mut found, Some(legacy), |name| name == legacy);
            assert_eq!(found.devices[1..], [device(legacy, legacy)], "{legacy}");
        }
        // One that no longer opens stays out, for settings to call missing.
        let mut found = listed.clone();
        keep_configured(&mut found, Some("front:CARD=Gone,DEV=0"), |_| false);
        assert_eq!(found, listed);
        // A listed one is not listed twice, nor is anything without settings.
        let mut found = listed.clone();
        keep_configured(&mut found, Some(&listed.devices[0].id), |_| true);
        keep_configured(&mut found, None, |_| true);
        assert_eq!(found, listed);
    }

    #[test]
    fn an_older_sysdefault_name_is_its_card_without_a_sound_server() {
        let dir = asound(CARDS, &["card2/pcm0c"]);
        let mut found = cards(dir.path());
        keep_configured(&mut found, Some("sysdefault:CARD=MV7"), |_| true);
        assert_eq!(
            found.devices,
            [device("sysdefault:CARD=MV7", "Shure Inc Shure MV7")]
        );
    }
}

//! Insertion by pasting: earwig puts the text on the clipboard and in the
//! primary selection (through data-control, or X11 through Xwayland on GNOME),
//! served from its own thread so it outlives the paste, then presses
//! Shift+Insert (on the compositor's virtual keyboard, or on its own through
//! uinput with keyboard access). Every app pastes on Shift+Insert, whatever the
//! keyboard layout: editors and browsers from the clipboard, terminals from the
//! primary selection. Both keep the text.

use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use earwig_keys::PasteKeyboard;
use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy as _, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use xkbcommon::xkb;

use self::x11::X11Clipboard;
use super::wayland::Worker;
use super::{InsertError, Inserter, MIME_TYPES};
use crate::desktop::DesktopCaps;
use crate::events::InsertMethod;

mod x11;

/// How long a held Ctrl, Alt or Super may hold up Shift+Insert on earwig's
/// own keyboard, which it would turn into another shortcut.
const MODIFIER_WAIT: Duration = Duration::from_secs(1);

/// The virtual keyboard's keymap: Shift and Insert alone, self-contained so
/// compiling it needs no XKB data files, at their evdev positions (+8). GTK
/// looks keys up below the highest keycode only, so one more key sits above.
const KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes "earwig" {
        <LFSH> = 50;
        <INS> = 118;
        <I255> = 255;
    };
    xkb_types "earwig" {
        type "ONE_LEVEL" {
            modifiers = none;
            level_name[Level1] = "Any";
        };
    };
    xkb_compatibility "earwig" {
        interpret Shift_L { action = SetMods(modifiers = Shift); };
    };
    xkb_symbols "earwig" {
        key <LFSH> { type = "ONE_LEVEL", [ Shift_L ] };
        key <INS> { type = "ONE_LEVEL", [ Insert ] };
        modifier_map Shift { <LFSH> };
    };
};
"#;

pub struct PasteInserter {
    keyboard: Keyboard,
    selections: Selections,
}

enum Keyboard {
    /// The compositor's (wlroots, niri, Hyprland, COSMIC).
    Virtual(Worker<VirtualKeyboard>),
    /// earwig's own, with keyboard access (GNOME, KDE).
    Uinput(PasteKeyboard),
}

impl PasteInserter {
    /// The compositor's virtual keyboard where it offers one, else earwig's
    /// own, which fails without write access to `/dev/uinput`.
    pub fn connect(caps: &DesktopCaps) -> anyhow::Result<Self> {
        let keyboard = if caps.virtual_keyboard {
            Keyboard::Virtual(Worker::spawn("earwig-keyboard", VirtualKeyboard::new)?)
        } else {
            Keyboard::Uinput(PasteKeyboard::new().context("making a keyboard through /dev/uinput")?)
        };
        Ok(Self {
            keyboard,
            selections: Selections::connect(caps)?,
        })
    }

    fn method(&self) -> InsertMethod {
        match self.keyboard {
            Keyboard::Virtual(_) => InsertMethod::Paste,
            Keyboard::Uinput(_) => InsertMethod::UinputPaste,
        }
    }
}

impl Inserter for PasteInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![self.method()]
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        self.selections.set(text, true)?;
        match &self.keyboard {
            Keyboard::Virtual(keyboard) => keyboard.call(VirtualKeyboard::shift_insert)??,
            Keyboard::Uinput(keyboard) => {
                wait_for_modifiers()?;
                keyboard.shift_insert().context("pressing Shift+Insert")?;
            }
        }
        Ok(self.method())
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        self.selections.set(text, false)
    }
}

/// Wait until the user lets go of Ctrl, Alt and Super; fail if they don't.
fn wait_for_modifiers() -> anyhow::Result<()> {
    let deadline = Instant::now() + MODIFIER_WAIT;
    while earwig_keys::modifier_held() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "Ctrl, Alt or Super was held down"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// What owns the clipboard and the primary selection.
pub(super) struct Selections(Owner);

enum Owner {
    DataControl(Worker<DataControl>),
    X11(X11Clipboard),
}

impl Selections {
    /// Through data-control where the compositor offers it, else through
    /// Xwayland on GNOME.
    pub(super) fn connect(caps: &DesktopCaps) -> anyhow::Result<Self> {
        let owner = if caps.data_control {
            Owner::DataControl(Worker::spawn("earwig-selections", DataControl::new)?)
        } else if mutter(caps) {
            Owner::X11(X11Clipboard::connect()?)
        } else {
            anyhow::bail!(
                "the compositor offers no data-control, and this is no GNOME with Xwayland"
            )
        };
        Ok(Self(owner))
    }

    /// Own the clipboard with `text`, and the primary selection too with
    /// `primary`; returns once the compositor has them.
    pub(super) fn set(&self, text: &str, primary: bool) -> Result<(), InsertError> {
        match &self.0 {
            Owner::DataControl(worker) => {
                let text: Arc<str> = text.into();
                worker.call(move |state, queue| -> anyhow::Result<()> {
                    state.set(&text, primary, &queue.handle())?;
                    queue.roundtrip(state).context("clipboard roundtrip")?;
                    Ok(())
                })??;
            }
            Owner::X11(clipboard) => clipboard.set(text, primary)?,
        }
        Ok(())
    }
}

/// The clipboard alone, where nothing can press the keys.
impl Inserter for Selections {
    fn methods(&self) -> Vec<InsertMethod> {
        Vec::new()
    }

    fn insert(&mut self, _: &str) -> Result<InsertMethod, InsertError> {
        Err(InsertError::NotApplicable(
            "no keyboard to paste with".into(),
        ))
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        self.set(text, false)
    }
}

/// Only Mutter is known to hand an X11 selection to Wayland clients.
fn mutter(caps: &DesktopCaps) -> bool {
    caps.wayland && caps.x11 && caps.desktop.split(':').any(|name| name == "gnome")
}

/// ext and wlr data-control are message-for-message the same; ext preferred.
/// The device is `None` after the compositor sent `finished`.
enum DataControl {
    Ext {
        manager: ExtDataControlManagerV1,
        device: Option<ExtDataControlDeviceV1>,
    },
    Wlr {
        manager: ZwlrDataControlManagerV1,
        device: Option<ZwlrDataControlDeviceV1>,
    },
}

impl DataControl {
    fn new(globals: &GlobalList, qh: &QueueHandle<Self>) -> anyhow::Result<Self> {
        let seat: WlSeat = globals.bind(qh, 1..=1, ()).context("no wl_seat")?;
        if let Ok(manager) = globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ()) {
            let device = manager.get_data_device(&seat, qh, ());
            return Ok(Self::Ext {
                manager,
                device: Some(device),
            });
        }
        let manager: ZwlrDataControlManagerV1 = globals.bind(qh, 1..=2, ()).context(
            "the compositor offers neither ext_data_control_manager_v1 nor zwlr_data_control_manager_v1",
        )?;
        let device = manager.get_data_device(&seat, qh, ());
        Ok(Self::Wlr {
            manager,
            device: Some(device),
        })
    }

    /// Own the clipboard selection with `text` until another client takes
    /// it, and the primary selection too with `primary`.
    fn set(&self, text: &Arc<str>, primary: bool, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        macro_rules! offer {
            ($manager:expr, $device:expr) => {{
                let source = || {
                    let source = $manager.create_data_source(qh, text.clone());
                    for mime in MIME_TYPES {
                        source.offer((*mime).to_owned());
                    }
                    source
                };
                $device.set_selection(Some(&source()));
                if primary {
                    $device.set_primary_selection(Some(&source()));
                }
            }};
        }
        match self {
            Self::Ext {
                manager,
                device: Some(device),
            } => offer!(manager, device),
            Self::Wlr {
                manager,
                device: Some(device),
            } => {
                anyhow::ensure!(
                    !primary || device.version() >= 2,
                    "the compositor's data-control has no primary selection"
                );
                offer!(manager, device);
            }
            _ => anyhow::bail!("the compositor withdrew the clipboard device"),
        }
        Ok(())
    }
}

/// Write `text` to a paste target's pipe off the Wayland thread, so a reader
/// that never drains it cannot stall the connection.
fn serve_transfer(text: Arc<str>, fd: OwnedFd) {
    let spawned = thread::Builder::new()
        .name("earwig-clipboard".into())
        .spawn(move || {
            if let Err(err) = File::from(fd).write_all(text.as_bytes()) {
                log::debug!("clipboard transfer ended early: {err}");
            }
        });
    if let Err(err) = spawned {
        log::warn!("could not serve a clipboard transfer: {err}");
    }
}

/// Dispatch for one data-control flavour: serve our sources, release other
/// clients' offers, and forget the device when it finishes.
macro_rules! data_control_dispatch {
    ($variant:ident, $device_mod:ident, $Device:ty, $source_mod:ident, $Source:ty, $Offer:ty) => {
        impl Dispatch<$Device, ()> for DataControl {
            fn event(
                state: &mut Self,
                device: &$Device,
                event: $device_mod::Event,
                (): &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $device_mod::Event::Selection { id: Some(offer) }
                    | $device_mod::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                    $device_mod::Event::Finished => {
                        log::warn!("the compositor withdrew the clipboard device");
                        device.destroy();
                        if let DataControl::$variant { device: slot, .. } = state {
                            *slot = None;
                        }
                    }
                    _ => {}
                }
            }

            wayland_client::event_created_child!(DataControl, $Device, [
                $device_mod::EVT_DATA_OFFER_OPCODE => ($Offer, ()),
            ]);
        }

        impl Dispatch<$Source, Arc<str>> for DataControl {
            fn event(
                _: &mut Self,
                source: &$Source,
                event: $source_mod::Event,
                text: &Arc<str>,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $source_mod::Event::Send { fd, .. } => serve_transfer(text.clone(), fd),
                    // Replaced by another selection.
                    $source_mod::Event::Cancelled => source.destroy(),
                    _ => {}
                }
            }
        }

        wayland_client::delegate_noop!(DataControl: ignore $Offer);
    };
}

data_control_dispatch!(
    Ext,
    ext_data_control_device_v1,
    ExtDataControlDeviceV1,
    ext_data_control_source_v1,
    ExtDataControlSourceV1,
    ExtDataControlOfferV1
);
data_control_dispatch!(
    Wlr,
    zwlr_data_control_device_v1,
    ZwlrDataControlDeviceV1,
    zwlr_data_control_source_v1,
    ZwlrDataControlSourceV1,
    ZwlrDataControlOfferV1
);

wayland_client::delegate_noop!(DataControl: ignore WlSeat);
wayland_client::delegate_noop!(DataControl: ExtDataControlManagerV1);
wayland_client::delegate_noop!(DataControl: ZwlrDataControlManagerV1);

impl Dispatch<WlRegistry, GlobalListContents> for DataControl {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// evdev keycodes of [`KEYMAP`]'s keys, and Shift's bit in its modifier mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Keys {
    shift: u32,
    shift_mask: u32,
    insert: u32,
}

struct VirtualKeyboard {
    device: ZwpVirtualKeyboardV1,
    keys: Keys,
}

impl VirtualKeyboard {
    fn new(globals: &GlobalList, qh: &QueueHandle<Self>) -> anyhow::Result<Self> {
        let seat: WlSeat = globals.bind(qh, 1..=1, ()).context("no wl_seat")?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(qh, 1..=1, ())
            .context("the compositor does not offer zwp_virtual_keyboard_manager_v1")?;
        let (keymap, keys) = compile_keymap()?;
        let fd = rustix::fs::memfd_create("earwig-keymap", rustix::fs::MemfdFlags::CLOEXEC)
            .context("memfd for the keymap")?;
        let mut file = File::from(fd);
        file.write_all(keymap.as_bytes())
            .and_then(|()| file.write_all(b"\0"))
            .context("writing the keymap")?;
        let device = manager.create_virtual_keyboard(&seat, qh, ());
        // The size includes the NUL terminator, like wl_keyboard.keymap.
        device.keymap(
            KeymapFormat::XkbV1.into(),
            file.as_fd(),
            u32::try_from(keymap.len() + 1).context("keymap size")?,
        );
        Ok(Self { device, keys })
    }

    /// Press and release Shift+Insert. Shift goes out both as a key and as the
    /// `modifiers` request: wlroots applies only the request, niri tracks the
    /// keys.
    fn shift_insert(&mut self, queue: &mut EventQueue<Self>) -> anyhow::Result<()> {
        let Keys {
            shift,
            shift_mask,
            insert,
        } = self.keys;
        let time = now_ms();
        let pressed = KeyState::Pressed.into();
        let released = KeyState::Released.into();
        self.device.key(time, shift, pressed);
        self.device.modifiers(shift_mask, 0, 0, 0);
        self.device.key(time, insert, pressed);
        self.device.key(time, insert, released);
        self.device.key(time, shift, released);
        self.device.modifiers(0, 0, 0, 0);
        queue.roundtrip(self).context("Shift+Insert roundtrip")?;
        Ok(())
    }
}

wayland_client::delegate_noop!(VirtualKeyboard: ignore WlSeat);
wayland_client::delegate_noop!(VirtualKeyboard: ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(VirtualKeyboard: ZwpVirtualKeyboardV1);

impl Dispatch<WlRegistry, GlobalListContents> for VirtualKeyboard {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Compile [`KEYMAP`] and find its keys: the text to upload (as compositors
/// re-serialize it) and the keys' evdev codes.
fn compile_keymap() -> anyhow::Result<(String, Keys)> {
    let context =
        xkb::Context::new(xkb::CONTEXT_NO_DEFAULT_INCLUDES | xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        KEYMAP.to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("compiling the paste keymap")?;
    let key = |sym: xkb::Keysym| -> anyhow::Result<u32> {
        (keymap.min_keycode().raw()..=keymap.max_keycode().raw())
            .find(|&code| keymap.key_get_syms_by_level(xkb::Keycode::new(code), 0, 0) == [sym])
            .map(|code| code - 8)
            .with_context(|| format!("keysym {sym:?} missing from the paste keymap"))
    };
    let shift = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
    anyhow::ensure!(shift < 32, "Shift missing from the paste keymap");
    let keys = Keys {
        shift: key(xkb::Keysym::Shift_L)?,
        shift_mask: 1 << shift,
        insert: key(xkb::Keysym::Insert)?,
    };
    Ok((keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1), keys))
}

/// `CLOCK_MONOTONIC` in milliseconds, the clock of Wayland input timestamps.
fn now_ms() -> u32 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let ms = now
        .tv_sec
        .wrapping_mul(1000)
        .wrapping_add(now.tv_nsec / 1_000_000);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Wayland timestamps are wrapping 32-bit milliseconds"
    )]
    let ms = ms as u32;
    ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_has_shift_and_insert_at_their_evdev_positions() {
        let (text, keys) = compile_keymap().unwrap();
        // KEY_LEFTSHIFT, KEY_INSERT; Shift the first real modifier.
        assert_eq!(
            keys,
            Keys {
                shift: 42,
                shift_mask: 1 << 0,
                insert: 110,
            }
        );
        // What a compositor receives must compile on its own, without XKB data.
        let context = xkb::Context::new(xkb::CONTEXT_NO_DEFAULT_INCLUDES);
        let reparsed = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("serialized keymap compiles");
        assert!(reparsed.max_keycode().raw() > keys.insert + 8);
        let mut state = xkb::State::new(&reparsed);
        state.update_key(xkb::Keycode::new(keys.shift + 8), xkb::KeyDirection::Down);
        assert_eq!(
            state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            keys.shift_mask
        );
        assert_eq!(
            state.key_get_one_sym(xkb::Keycode::new(keys.insert + 8)),
            xkb::Keysym::Insert
        );
    }

    #[test]
    fn x11_selections_only_on_gnome_under_wayland_with_xwayland() {
        let gnome = DesktopCaps {
            desktop: "ubuntu:GNOME".to_lowercase(),
            wayland: true,
            x11: true,
            ..DesktopCaps::default()
        };
        assert!(mutter(&gnome));
        assert!(!mutter(&DesktopCaps {
            x11: false,
            ..gnome.clone()
        }));
        assert!(!mutter(&DesktopCaps {
            wayland: false,
            ..gnome.clone()
        }));
        assert!(!mutter(&DesktopCaps {
            desktop: "weston".into(),
            ..gnome
        }));
    }
}

//! Insertion by clipboard and one paste chord (wlroots compositors, niri,
//! Hyprland): the text becomes the clipboard selection through data-control,
//! then a `zwp_virtual_keyboard_v1` presses Ctrl+V, or Ctrl+Shift+V when the
//! activated toplevel is a terminal. Works for clients without text-input
//! (Electron, XWayland, most terminals' default builds).
//!
//! The selection is served from earwig's own Wayland thread until another
//! client replaces it, so the transcript stays on the clipboard after the
//! paste. Requests reach the compositor in order on one connection, so the
//! focused client sees the new selection before the chord; no delay is needed.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;

use anyhow::Context as _;
use wayland_client::backend::ObjectId;
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
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use xkbcommon::xkb;

use super::wayland::Worker;
use super::{InsertError, Inserter};
use crate::events::InsertMethod;

/// Text targets offered on the clipboard, most specific first.
const MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];

/// App ids (compared ASCII case-insensitively) that paste with Ctrl+Shift+V.
/// `[insert] terminals` adds to this list.
pub const TERMINALS: &[&str] = &[
    "Alacritty",
    "com.gexperts.Tilix",
    "com.mitchellh.ghostty",
    "com.raggesilver.BlackBox",
    "dev.warp.Warp",
    "foot",
    "footclient",
    "io.elementary.terminal",
    "kgx",
    "kitty",
    "konsole",
    "org.gnome.Console",
    "org.gnome.Ptyxis",
    "org.gnome.Terminal",
    "org.kde.konsole",
    "org.wezfurlong.wezterm",
    "rio",
    "st",
    "st-256color",
    "terminator",
    "URxvt",
    "xfce4-terminal",
    "xterm",
];

/// The virtual keyboard's keymap: just the three keys of the paste chords,
/// self-contained so compiling it needs no XKB data files. Keycodes are the
/// evdev positions of Left Ctrl, Left Shift and V (+8), so clients that match
/// shortcuts by key position see the usual keys too.
const KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes "earwig" {
        minimum = 8;
        maximum = 255;
        <LCTL> = 37;
        <LFSH> = 50;
        <AB04> = 55;
    };
    xkb_types "earwig" {
        type "ONE_LEVEL" {
            modifiers = none;
            level_name[Level1] = "Any";
        };
        type "ALPHABETIC" {
            modifiers = Shift + Lock;
            map[Shift] = Level2;
            map[Lock] = Level2;
            level_name[Level1] = "Base";
            level_name[Level2] = "Caps";
        };
    };
    xkb_compatibility "earwig" {
        interpret Control_L { action = SetMods(modifiers = Control); };
        interpret Shift_L { action = SetMods(modifiers = Shift); };
    };
    xkb_symbols "earwig" {
        key <LCTL> { type = "ONE_LEVEL", [ Control_L ] };
        key <LFSH> { type = "ONE_LEVEL", [ Shift_L ] };
        key <AB04> { type = "ALPHABETIC", [ v, V ] };
        modifier_map Control { <LCTL> };
        modifier_map Shift { <LFSH> };
    };
};
"#;

pub struct PasteInserter {
    worker: Worker<State>,
    /// `false`: clipboard only, `insert` is not applicable.
    chord: bool,
}

impl PasteInserter {
    /// Clipboard plus paste chord. Needs data-control and a virtual keyboard;
    /// wlr foreign-toplevel, when offered, picks the terminal chord.
    /// `terminals` are app ids added to [`TERMINALS`].
    pub fn connect(terminals: &[String]) -> anyhow::Result<Self> {
        Self::spawn(Some(terminals.to_vec()))
    }

    /// Clipboard only, for [`Inserter::copy_to_clipboard`] where no paste
    /// chord can be sent (or another backend inserts).
    pub fn clipboard_only() -> anyhow::Result<Self> {
        Self::spawn(None)
    }

    fn spawn(terminals: Option<Vec<String>>) -> anyhow::Result<Self> {
        let chord = terminals.is_some();
        let worker = Worker::spawn("earwig-paste", move |globals, qh| {
            State::new(globals, qh, terminals)
        })?;
        Ok(Self { worker, chord })
    }
}

impl Inserter for PasteInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        if self.chord {
            vec![InsertMethod::Paste]
        } else {
            Vec::new()
        }
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        if !self.chord {
            return Err(InsertError::NotApplicable(
                "no virtual keyboard for a paste chord".into(),
            ));
        }
        let text: Arc<str> = text.into();
        self.worker
            .call(move |state, queue| state.paste(text, queue))?
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        let text: Arc<str> = text.into();
        self.worker
            .call(move |state, queue| -> anyhow::Result<()> {
                state.set_clipboard(text, &queue.handle())?;
                queue.roundtrip(state).context("clipboard roundtrip")?;
                Ok(())
            })??;
        Ok(())
    }
}

struct State {
    clipboard: Clipboard,
    /// `None` for clipboard-only.
    keyboard: Option<Keyboard>,
    /// Toplevels from wlr foreign-toplevel; empty without it.
    toplevels: HashMap<ObjectId, ToplevelState>,
    /// Added to [`TERMINALS`].
    terminals: Vec<String>,
}

/// ext and wlr data-control are message-for-message the same; ext preferred.
enum Clipboard {
    Ext {
        manager: ExtDataControlManagerV1,
        /// `None` after the compositor sent `finished`.
        device: Option<ExtDataControlDeviceV1>,
    },
    Wlr {
        manager: ZwlrDataControlManagerV1,
        device: Option<ZwlrDataControlDeviceV1>,
    },
}

impl State {
    fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        terminals: Option<Vec<String>>,
    ) -> anyhow::Result<Self> {
        let seat: WlSeat = globals.bind(qh, 1..=1, ()).context("no wl_seat")?;
        let clipboard = if let Ok(manager) =
            globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ())
        {
            let device = manager.get_data_device(&seat, qh, ());
            Clipboard::Ext {
                manager,
                device: Some(device),
            }
        } else {
            let manager: ZwlrDataControlManagerV1 = globals.bind(qh, 1..=2, ()).context(
                "the compositor offers neither ext_data_control_manager_v1 nor zwlr_data_control_manager_v1",
            )?;
            let device = manager.get_data_device(&seat, qh, ());
            Clipboard::Wlr {
                manager,
                device: Some(device),
            }
        };
        let keyboard = if terminals.is_some() {
            let manager: ZwpVirtualKeyboardManagerV1 = globals
                .bind(qh, 1..=1, ())
                .context("the compositor does not offer zwp_virtual_keyboard_manager_v1")?;
            if globals
                .bind::<ZwlrForeignToplevelManagerV1, _, _>(qh, 1..=3, ())
                .is_err()
            {
                log::info!("no wlr foreign-toplevel: pasting with Ctrl+V everywhere");
            }
            Some(Keyboard::new(&manager, &seat, qh)?)
        } else {
            None
        };
        Ok(Self {
            clipboard,
            keyboard,
            toplevels: HashMap::new(),
            terminals: terminals.unwrap_or_default(),
        })
    }

    /// Own the clipboard selection with `text` until another client takes it.
    fn set_clipboard(&self, text: Arc<str>, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        macro_rules! offer {
            ($manager:expr, $device:expr) => {{
                let source = $manager.create_data_source(qh, text);
                for mime in MIME_TYPES {
                    source.offer((*mime).to_owned());
                }
                $device.set_selection(Some(&source));
            }};
        }
        match &self.clipboard {
            Clipboard::Ext {
                manager,
                device: Some(device),
            } => offer!(manager, device),
            Clipboard::Wlr {
                manager,
                device: Some(device),
            } => offer!(manager, device),
            _ => anyhow::bail!("the compositor withdrew the clipboard device"),
        }
        Ok(())
    }

    fn paste(
        &mut self,
        text: Arc<str>,
        queue: &mut EventQueue<Self>,
    ) -> Result<InsertMethod, InsertError> {
        self.set_clipboard(text, &queue.handle())?;
        // Applies the selection and brings the activated toplevel up to date.
        queue.roundtrip(self).context("clipboard roundtrip")?;
        let app_id = self.activated_app_id();
        let terminal = app_id.is_some_and(|app_id| is_terminal(app_id, &self.terminals));
        log::debug!(
            "pasting into {app_id:?} with {}",
            if terminal { "Ctrl+Shift+V" } else { "Ctrl+V" }
        );
        self.keyboard
            .as_ref()
            .context("no virtual keyboard")?
            .paste_chord(terminal);
        queue.roundtrip(self).context("paste chord roundtrip")?;
        Ok(InsertMethod::Paste)
    }

    fn activated_app_id(&self) -> Option<&str> {
        self.toplevels
            .values()
            .find(|toplevel| toplevel.current.activated)
            .map(|toplevel| toplevel.current.app_id.as_str())
    }
}

fn is_terminal(app_id: &str, extra: &[String]) -> bool {
    TERMINALS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|terminal| terminal.eq_ignore_ascii_case(app_id))
}

/// Write `text` to a paste target's pipe off the Wayland thread, so a reader
/// that never drains it cannot stall the connection.
fn serve_transfer(text: Arc<str>, fd: OwnedFd) {
    let spawned = std::thread::Builder::new()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Modifier {
    /// evdev keycode.
    key: u32,
    /// Bit in the keymap's modifier mask.
    mask: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChordKeys {
    ctrl: Modifier,
    shift: Modifier,
    /// evdev keycode of `v`.
    v: u32,
}

struct Keyboard {
    device: ZwpVirtualKeyboardV1,
    keys: ChordKeys,
}

impl Keyboard {
    fn new(
        manager: &ZwpVirtualKeyboardManagerV1,
        seat: &WlSeat,
        qh: &QueueHandle<State>,
    ) -> anyhow::Result<Self> {
        let (keymap, keys) = compile_keymap()?;
        let fd = rustix::fs::memfd_create("earwig-keymap", rustix::fs::MemfdFlags::CLOEXEC)
            .context("memfd for the keymap")?;
        let mut file = File::from(fd);
        file.write_all(keymap.as_bytes())
            .and_then(|()| file.write_all(b"\0"))
            .context("writing the keymap")?;
        let device = manager.create_virtual_keyboard(seat, qh, ());
        // The size includes the NUL terminator, like wl_keyboard.keymap.
        device.keymap(
            KeymapFormat::XkbV1.into(),
            file.as_fd(),
            u32::try_from(keymap.len() + 1).context("keymap size")?,
        );
        Ok(Self { device, keys })
    }

    /// Press and release Ctrl(+Shift)+V. Modifiers are sent both as keys and
    /// as the `modifiers` request: wlroots applies only the request, niri
    /// tracks the keys.
    fn paste_chord(&self, terminal: bool) {
        let ChordKeys { ctrl, shift, v } = self.keys;
        let held: &[Modifier] = if terminal { &[ctrl, shift] } else { &[ctrl] };
        let time = now_ms();
        let pressed = KeyState::Pressed.into();
        let released = KeyState::Released.into();
        let mut mods = 0;
        for modifier in held {
            self.device.key(time, modifier.key, pressed);
            mods |= modifier.mask;
            self.device.modifiers(mods, 0, 0, 0);
        }
        self.device.key(time, v, pressed);
        self.device.key(time, v, released);
        for modifier in held.iter().rev() {
            self.device.key(time, modifier.key, released);
            mods &= !modifier.mask;
            self.device.modifiers(mods, 0, 0, 0);
        }
    }
}

/// Compile [`KEYMAP`] and find the chord keys in it: the text to upload
/// (as compositors re-serialize it) and evdev codes plus modifier masks.
fn compile_keymap() -> anyhow::Result<(String, ChordKeys)> {
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
    let mask = |name: &str| -> anyhow::Result<u32> {
        let index = keymap.mod_get_index(name);
        anyhow::ensure!(index < 32, "modifier {name} missing from the paste keymap");
        Ok(1 << index)
    };
    let keys = ChordKeys {
        ctrl: Modifier {
            key: key(xkb::Keysym::Control_L)?,
            mask: mask(xkb::MOD_NAME_CTRL)?,
        },
        shift: Modifier {
            key: key(xkb::Keysym::Shift_L)?,
            mask: mask(xkb::MOD_NAME_SHIFT)?,
        },
        v: key(xkb::Keysym::v)?,
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

#[derive(Clone, Debug, Default)]
struct Toplevel {
    app_id: String,
    activated: bool,
}

/// wlr foreign-toplevel state is double-buffered: events fill `pending`,
/// `done` applies it.
#[derive(Debug, Default)]
struct ToplevelState {
    pending: Toplevel,
    current: Toplevel,
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        if let Event::Closed = event {
            state.toplevels.remove(&handle.id());
            handle.destroy();
            return;
        }
        let toplevel = state.toplevels.entry(handle.id()).or_default();
        match event {
            Event::AppId { app_id } => toplevel.pending.app_id = app_id,
            Event::State { state } => {
                let activated = u32::from(zwlr_foreign_toplevel_handle_v1::State::Activated);
                toplevel.pending.activated = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|raw| u32::from_ne_bytes(*raw) == activated);
            }
            Event::Done => toplevel.current = toplevel.pending.clone(),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Finished = event {
            state.toplevels.clear();
        }
    }

    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

/// Dispatch for one data-control flavour: serve our sources, release other
/// clients' offers (never read), and forget the device when it finishes.
macro_rules! data_control_dispatch {
    ($variant:ident, $device_mod:ident, $Device:ty, $source_mod:ident, $Source:ty, $Offer:ty) => {
        impl Dispatch<$Device, ()> for State {
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
                        if let Clipboard::$variant { device: slot, .. } = &mut state.clipboard {
                            *slot = None;
                        }
                    }
                    _ => {}
                }
            }

            wayland_client::event_created_child!(State, $Device, [
                $device_mod::EVT_DATA_OFFER_OPCODE => ($Offer, ()),
            ]);
        }

        impl Dispatch<$Source, Arc<str>> for State {
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

        wayland_client::delegate_noop!(State: ignore $Offer);
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

wayland_client::delegate_noop!(State: ignore WlSeat);
wayland_client::delegate_noop!(State: ExtDataControlManagerV1);
wayland_client::delegate_noop!(State: ZwlrDataControlManagerV1);
wayland_client::delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(State: ZwpVirtualKeyboardV1);

impl Dispatch<WlRegistry, GlobalListContents> for State {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_has_the_chord_keys_at_their_evdev_positions() {
        let (text, keys) = compile_keymap().unwrap();
        // KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V; Control and Shift real mods.
        assert_eq!(
            keys,
            ChordKeys {
                ctrl: Modifier {
                    key: 29,
                    mask: 1 << 2
                },
                shift: Modifier {
                    key: 42,
                    mask: 1 << 0
                },
                v: 47,
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
        let mut state = xkb::State::new(&reparsed);
        state.update_key(
            xkb::Keycode::new(keys.ctrl.key + 8),
            xkb::KeyDirection::Down,
        );
        state.update_key(
            xkb::Keycode::new(keys.shift.key + 8),
            xkb::KeyDirection::Down,
        );
        assert_eq!(
            state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            keys.ctrl.mask | keys.shift.mask
        );
        assert_eq!(
            state.key_get_one_sym(xkb::Keycode::new(keys.v + 8)),
            xkb::Keysym::V
        );
    }

    #[test]
    fn terminals_match_case_insensitively_and_include_configured_ones() {
        let extra = ["my.Custom.Term".to_owned()];
        assert!(is_terminal("foot", &[]));
        assert!(is_terminal("alacritty", &[]));
        assert!(is_terminal("XTerm", &[]));
        assert!(!is_terminal("firefox", &[]));
        assert!(!is_terminal("my.custom.term", &[]));
        assert!(is_terminal("my.custom.term", &extra));
    }
}

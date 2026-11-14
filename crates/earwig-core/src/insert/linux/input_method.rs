//! Insertion through `zwp_input_method_v2` (niri, sway, Hyprland): the text
//! reaches the focused field as a `commit_string`, the clipboard is left alone.
//! earwig never grabs the keyboard; if another input method (fcitx5, ibus)
//! holds the seat, the compositor says `unavailable` and this steps aside.

use anyhow::Context as _;
use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{
    self, ZwpInputMethodV2,
};

use super::wayland::Worker;
use super::{InsertError, Inserter};
use crate::events::InsertMethod;

/// Wayland messages are capped at 4096 bytes with the header, so longer text
/// goes out as several commits in one burst.
const MAX_COMMIT_BYTES: usize = 4000;

pub struct InputMethodInserter {
    worker: Worker<State>,
}

impl InputMethodInserter {
    /// Bind the default seat's input method on its own connection; fails when
    /// the protocol is missing or another input method holds the seat.
    pub fn connect() -> anyhow::Result<Self> {
        let worker = Worker::spawn("earwig-input-method", State::new)?;
        if !worker.call(|state, _| state.input_method.is_some())? {
            anyhow::bail!("another input method is bound to the seat");
        }
        Ok(Self { worker })
    }
}

impl Inserter for InputMethodInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![InsertMethod::InputMethod]
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        if text.contains('\0') {
            return Err(InsertError::NotApplicable(
                "text contains NUL, which a Wayland string cannot carry".into(),
            ));
        }
        let text = text.to_owned();
        self.worker
            .call(move |state, queue| state.commit(&text, queue))?
    }

    fn copy_to_clipboard(&mut self, _text: &str) -> Result<(), InsertError> {
        Err(InsertError::NotApplicable(
            "the input method has no clipboard".into(),
        ))
    }
}

struct State {
    /// `None` once the compositor said `unavailable`.
    input_method: Option<ZwpInputMethodV2>,
    /// `activate`/`deactivate` take effect on the next `done`.
    pending_active: bool,
    /// A focused text field has text input enabled.
    active: bool,
    /// Number of `done` events so far; `commit` must echo it.
    serial: u32,
}

impl State {
    fn new(globals: &GlobalList, qh: &QueueHandle<Self>) -> anyhow::Result<Self> {
        let seat: WlSeat = globals.bind(qh, 1..=1, ()).context("no wl_seat")?;
        let manager: ZwpInputMethodManagerV2 = globals
            .bind(qh, 1..=1, ())
            .context("the compositor does not offer zwp_input_method_manager_v2")?;
        Ok(Self {
            input_method: Some(manager.get_input_method(&seat, qh, ())),
            pending_active: false,
            active: false,
            serial: 0,
        })
    }

    fn commit(
        &mut self,
        text: &str,
        queue: &mut EventQueue<Self>,
    ) -> Result<InsertMethod, InsertError> {
        // Activation is sent asynchronously; catch up before deciding.
        queue.roundtrip(self).context("input method roundtrip")?;
        let Some(input_method) = &self.input_method else {
            return Err(InsertError::NotApplicable(
                "another input method took over the seat".into(),
            ));
        };
        if !self.active {
            return Err(InsertError::NotApplicable(
                "the focused window has no active text field".into(),
            ));
        }
        for chunk in chunks(text, MAX_COMMIT_BYTES) {
            input_method.commit_string(chunk.to_owned());
            input_method.commit(self.serial);
        }
        queue.roundtrip(self).context("input method roundtrip")?;
        Ok(InsertMethod::InputMethod)
    }
}

/// Split `text` into pieces of at most `max` bytes on char boundaries.
fn chunks(text: &str, max: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = if rest.len() <= max {
            rest.len()
        } else {
            // A limit below one char's width still has to make progress.
            match rest.floor_char_boundary(max) {
                0 => rest.chars().next().map_or(rest.len(), char::len_utf8),
                end => end,
            }
        };
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = true,
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::Done => {
                state.serial = state.serial.wrapping_add(1);
                state.active = state.pending_active;
            }
            zwp_input_method_v2::Event::Unavailable => {
                log::warn!("input method unavailable: another one holds the seat");
                proxy.destroy();
                state.input_method = None;
                state.active = false;
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(State: ignore WlSeat);

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
    use super::chunks;

    #[test]
    fn chunks_respect_the_byte_limit_and_char_boundaries() {
        // "é" is 2 bytes and "🦀" is 4: a 5-byte limit must not split either.
        assert_eq!(chunks("aé🦀bé", 5).collect::<Vec<_>>(), ["aé", "🦀b", "é"]);
        assert_eq!(chunks("hello", 4000).collect::<Vec<_>>(), ["hello"]);
        assert_eq!(chunks("", 4000).count(), 0);
    }
}

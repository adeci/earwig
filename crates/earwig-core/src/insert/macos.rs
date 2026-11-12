//! macOS: the text goes on the general pasteboard, then Cmd+V is pressed in
//! the focused app (terminals included). Pressing keys in other apps needs
//! Accessibility access. The text stays on the clipboard afterwards.

use anyhow::bail;

use super::{InsertError, Inserter};
use crate::config::{InsertConfig, InsertSetting};
use crate::desktop::DesktopCaps;
use crate::events::InsertMethod;
use crate::macos;
use crate::permissions::{self, Permission};

pub(super) fn backends(
    config: &InsertConfig,
    _caps: &DesktopCaps,
) -> anyhow::Result<Vec<Box<dyn Inserter>>> {
    match config.method {
        InsertSetting::Auto | InsertSetting::Paste => Ok(vec![Box::new(Paste)]),
        other => bail!("insertion method {other:?} does not exist on macOS; use paste"),
    }
}

struct Paste;

impl Inserter for Paste {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![InsertMethod::Paste]
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        // Without Accessibility the key press is dropped without a word.
        if !macos::accessibility_allowed() {
            macos::set_clipboard(text)?;
            return Err(InsertError::Failed(anyhow::anyhow!(
                permissions::missing_message(Permission::Accessibility)
            )));
        }
        macos::set_clipboard(text)?;
        macos::press_paste();
        Ok(InsertMethod::Paste)
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        Ok(macos::set_clipboard(text)?)
    }
}

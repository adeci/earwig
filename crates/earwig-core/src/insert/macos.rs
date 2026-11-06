//! macOS: no insertion method yet.

use super::Inserter;
use crate::config::InsertConfig;
use crate::desktop::DesktopCaps;

pub(super) fn backends(
    _config: &InsertConfig,
    _caps: &DesktopCaps,
) -> anyhow::Result<Vec<Box<dyn Inserter>>> {
    anyhow::bail!("earwig has no text insertion method for macOS yet")
}

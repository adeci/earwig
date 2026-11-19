//! Where the pill was last dragged, kept in the state directory while `[overlay] remember_position`
//! is on: its spot from the area's top-left, for the edge it hugged.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;

use earwig_core::config::{OverlayConfig, OverlayPosition};
use gpui_kit::{Pixels, Point, point, px};
use serde::{Deserialize, Serialize};

const FILE: &str = "overlay.json";

#[derive(Serialize, Deserialize)]
struct Left {
    position: OverlayPosition,
    x: f32,
    y: f32,
}

/// Where the pill was left on `config`'s edge, if the setting is on; off, it is forgotten.
pub(super) fn load(dir: &Path, config: &OverlayConfig) -> Option<Point<Pixels>> {
    if !config.remember_position {
        forget(dir);
        return None;
    }
    let left: Left = serde_json::from_slice(&fs::read(dir.join(FILE)).ok()?).ok()?;
    // A spot's y is its edge's: one left on the other edge means something else.
    (left.position == config.position).then(|| point(px(left.x), px(left.y)))
}

pub(super) fn save(dir: &Path, position: OverlayPosition, spot: Point<Pixels>) {
    let left = Left {
        position,
        x: f32::from(spot.x),
        y: f32::from(spot.y),
    };
    let saved = serde_json::to_vec(&left)
        .map_err(io::Error::from)
        .and_then(|json| {
            fs::create_dir_all(dir)?;
            fs::write(dir.join(FILE), json)
        });
    if let Err(e) = saved {
        log::warn!("remembering where the overlay was left: {e}");
    }
}

pub(crate) fn forget(dir: &Path) {
    if let Err(e) = fs::remove_file(dir.join(FILE))
        && e.kind() != ErrorKind::NotFound
    {
        log::warn!("forgetting where the overlay was left: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remembering(on: bool, position: OverlayPosition) -> OverlayConfig {
        OverlayConfig {
            remember_position: on,
            position,
            ..OverlayConfig::default()
        }
    }

    #[test]
    fn the_spot_comes_back_on_its_edge_while_the_setting_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let on = remembering(true, OverlayPosition::Bottom);
        assert_eq!(load(dir.path(), &on), None);
        let spot = point(px(120.5), px(400.));
        save(dir.path(), OverlayPosition::Bottom, spot);
        assert_eq!(load(dir.path(), &on), Some(spot));
        assert_eq!(
            load(dir.path(), &remembering(true, OverlayPosition::Top)),
            None
        );
    }

    #[test]
    fn switching_the_setting_off_forgets_the_spot() {
        let dir = tempfile::tempdir().unwrap();
        let on = remembering(true, OverlayPosition::Bottom);
        let off = remembering(false, OverlayPosition::Bottom);
        let spot = point(px(120.), px(400.));
        // In the config file, from the next overlay...
        save(dir.path(), OverlayPosition::Bottom, spot);
        assert_eq!(load(dir.path(), &off), None);
        assert_eq!(load(dir.path(), &on), None);
        // ...and in the settings window, at once.
        save(dir.path(), OverlayPosition::Bottom, spot);
        forget(dir.path());
        assert_eq!(load(dir.path(), &on), None);
    }
}

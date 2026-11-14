//! The desktop's accent colour from the XDG Settings portal.

use ashpd::desktop::Color;
use ashpd::desktop::settings::Settings;
use futures_lite::StreamExt as _;
use gpui_kit::{App, Hsla, Rgba};

pub(crate) fn follow(tx: &flume::Sender<Option<Hsla>>, cx: &App) {
    let tx = tx.clone();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = listen(&tx).await {
                log::debug!("no accent colour from the desktop portal: {e}");
            }
        })
        .detach();
}

async fn listen(tx: &flume::Sender<Option<Hsla>>) -> ashpd::Result<()> {
    let settings = Settings::new().await?;
    // Subscribe before reading, so no change slips between.
    let mut changes = settings.receive_accent_color_changed().await?;
    let _ = tx.send(settings.accent_color().await.ok().and_then(to_hsla));
    while let Some(color) = changes.next().await {
        if tx.send(to_hsla(color)).is_err() {
            break;
        }
    }
    Ok(())
}

/// The portal speaks sRGB in 0..1; anything outside means "no preference".
#[allow(clippy::cast_possible_truncation)]
fn to_hsla(color: Color) -> Option<Hsla> {
    let channels = [color.red(), color.green(), color.blue()];
    channels.iter().all(|c| (0. ..=1.).contains(c)).then(|| {
        Rgba {
            r: channels[0] as f32,
            g: channels[1] as f32,
            b: channels[2] as f32,
            a: 1.,
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_means_no_accent() {
        let blue = to_hsla(Color::from((0.208, 0.518, 0.894))).map(Rgba::from);
        assert!(blue.is_some_and(|c| (c.b - 0.894).abs() < 1e-3));
        assert_eq!(to_hsla(Color::from((-1., -1., -1.))), None);
    }
}

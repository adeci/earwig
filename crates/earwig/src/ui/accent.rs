//! The desktop's accent colour, `org.freedesktop.appearance accent-color`
//! from the XDG Settings portal. Dark or light comes from GPUI itself, which
//! follows the portal's `color-scheme`.

use ashpd::desktop::Color;
use ashpd::desktop::settings::Settings;
use futures_lite::StreamExt as _;
use gpui_kit::{App, Global, Hsla, Rgba};

/// `None` until the portal answers, and when the desktop has no accent.
#[derive(Default)]
struct SystemAccent(Option<Hsla>);

impl Global for SystemAccent {}

/// The desktop's accent colour, if it has one.
pub(crate) fn get(cx: &App) -> Option<Hsla> {
    cx.try_global::<SystemAccent>().and_then(|accent| accent.0)
}

/// Read the accent off the main thread and keep following it.
pub(crate) fn watch(cx: &mut App) {
    cx.set_global(SystemAccent::default());
    let (tx, rx) = flume::unbounded();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = listen(&tx).await {
                log::debug!("no accent colour from the desktop portal: {e}");
            }
        })
        .detach();
    cx.spawn(async move |cx| {
        while let Ok(accent) = rx.recv_async().await {
            cx.update(|cx| {
                cx.set_global(SystemAccent(accent));
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

async fn listen(tx: &flume::Sender<Option<Hsla>>) -> ashpd::Result<()> {
    let settings = Settings::new().await?;
    // Subscribe first so a change between the read and the subscription
    // is not lost.
    let mut changes = settings.receive_accent_color_changed().await?;
    let _ = tx.send(settings.accent_color().await.ok().and_then(to_hsla));
    while let Some(color) = changes.next().await {
        if tx.send(to_hsla(color)).is_err() {
            break;
        }
    }
    Ok(())
}

/// The portal speaks sRGB in 0..1; anything outside that means "no
/// preference".
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

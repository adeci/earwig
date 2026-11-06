//! The desktop's accent colour, as the OS reports it (see
//! [`platform::follow_accent`]). Dark or light is earwig's own
//! `[appearance] theme`, not the desktop's colour scheme.

use gpui_kit::{App, Global, Hsla};

use crate::ui::platform;

/// `None` until the desktop answers, and when it has no accent.
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
    platform::follow_accent(&tx, cx);
    drop(tx);
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

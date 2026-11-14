//! Linux: XDG desktop name, Wayland globals, X11 and the XDG portals.

use std::time::Duration;

use ashpd::zbus;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use super::DesktopCaps;

/// As long as D-Bus waits for an activated service: at login the portal only
/// answers once its backends are up (about 5 s for KDE's).
const PORTAL_TIMEOUT: Duration = Duration::from_secs(25);

pub(super) fn probe() -> DesktopCaps {
    let mut caps = DesktopCaps {
        desktop: std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase(),
        x11: std::env::var_os("DISPLAY").is_some(),
        ..DesktopCaps::default()
    };
    // The portal may need D-Bus activation; ask it while listing globals.
    let portal = std::thread::Builder::new()
        .name("earwig-probe-portal".into())
        .spawn(portal_interfaces);
    match wayland_globals() {
        Ok(globals) => {
            let has = |name: &str| globals.iter().any(|global| global == name);
            caps.wayland = true;
            caps.layer_shell = has(ZwlrLayerShellV1::interface().name);
            caps.input_method = has(ZwpInputMethodManagerV2::interface().name);
            caps.virtual_keyboard = has(ZwpVirtualKeyboardManagerV1::interface().name);
            caps.data_control = has(ExtDataControlManagerV1::interface().name)
                || has(ZwlrDataControlManagerV1::interface().name);
        }
        Err(err) => log::info!("no Wayland session: {err:#}"),
    }
    match portal.map(std::thread::JoinHandle::join) {
        Ok(Ok(Ok(interfaces))) => {
            let has = |name: &str| interfaces.contains(&format!("name=\"{name}\""));
            caps.portal_global_shortcuts = has("org.freedesktop.portal.GlobalShortcuts");
            caps.portal_remote_desktop = has("org.freedesktop.portal.RemoteDesktop");
        }
        Ok(Ok(Err(err))) => log::info!("no desktop portal: {err:#}"),
        Ok(Err(_)) => log::warn!("the portal probe panicked"),
        Err(err) => log::warn!("could not probe the desktop portal: {err}"),
    }
    log::debug!("desktop capabilities: {caps:?}");
    caps
}

/// Interface names of the compositor's globals, after one roundtrip.
fn wayland_globals() -> anyhow::Result<Vec<String>> {
    struct Registry;
    impl Dispatch<WlRegistry, GlobalListContents> for Registry {
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

    let conn = Connection::connect_to_env()?;
    let (globals, _queue) = registry_queue_init::<Registry>(&conn)?;
    Ok(globals
        .contents()
        .with_list(|list| list.iter().map(|global| global.interface.clone()).collect()))
}

/// Introspection XML of the portal frontend's desktop object, which lists
/// only the portals some backend implements.
fn portal_interfaces() -> anyhow::Result<String> {
    let introspect = async {
        let conn = zbus::Connection::session().await?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.DBus.Introspectable"),
                "Introspect",
                &(),
            )
            .await?;
        anyhow::Ok(reply.body().deserialize::<String>()?)
    };
    let timeout = async {
        async_io::Timer::after(PORTAL_TIMEOUT).await;
        anyhow::bail!("no answer within {PORTAL_TIMEOUT:?}")
    };
    async_io::block_on(futures_lite::future::or(introspect, timeout))
}

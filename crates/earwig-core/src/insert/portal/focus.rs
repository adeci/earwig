//! Whether a terminal has keyboard focus, from the accessibility bus
//! (AT-SPI). GNOME tells portal clients nothing about the focused app, yet
//! terminals paste with Ctrl+Shift+V where everything else uses Ctrl+V.
//!
//! VTE terminals (GNOME Console, GNOME Terminal, Ptyxis) report the
//! `terminal` role. Terminals without accessibility support look like any
//! other app and get Ctrl+V.

use anyhow::Context as _;
use ashpd::zbus::{self, MatchRule, MessageStream, message};
use atspi_proxies::accessible::AccessibleProxy;
use atspi_proxies::bus::BusProxy;
use atspi_proxies::common::Role;
use atspi_proxies::registry::RegistryProxy;
use futures_lite::StreamExt as _;

/// Focus changes on the accessibility bus.
pub(super) struct FocusWatch {
    bus: zbus::Connection,
    events: MessageStream,
}

/// An accessible object that just gained focus.
pub(super) struct Focused {
    app: zbus::names::OwnedUniqueName,
    path: zbus::zvariant::OwnedObjectPath,
}

impl FocusWatch {
    pub(super) async fn connect() -> anyhow::Result<Self> {
        let session = zbus::Connection::session().await?;
        let address = BusProxy::new(&session)
            .await?
            .get_address()
            .await
            .context("no accessibility bus")?;
        let bus = zbus::connection::Builder::address(address.as_str())?
            .build()
            .await?;
        let rule = MatchRule::builder()
            .msg_type(message::Type::Signal)
            .interface("org.a11y.atspi.Event.Object")?
            .member("StateChanged")?
            .arg(0, "focused")?
            .build();
        let events = MessageStream::for_match_rule(rule, &bus, None).await?;
        // Toolkits that only emit events someone listens for (Qt) need this;
        // GTK emits them regardless.
        if let Err(e) = RegistryProxy::new(&bus)
            .await?
            .register_event("object:state-changed:focused")
            .await
        {
            log::debug!("registering for AT-SPI focus events: {e}");
        }
        Ok(Self { bus, events })
    }

    /// The next object that gains focus; `None` when the bus goes away.
    pub(super) async fn next(&mut self) -> Option<Focused> {
        while let Some(message) = self.events.next().await {
            let Ok(message) = message else { continue };
            let header = message.header();
            let (Some(app), Some(path)) = (header.sender(), header.path()) else {
                continue;
            };
            // Body: (kind, detail1, detail2, any_data, ...); detail1 is 1 when
            // the state was set.
            let body = message.body();
            let Ok(body) = body.deserialize::<zbus::zvariant::Structure>() else {
                continue;
            };
            let gained = matches!(body.fields().get(1), Some(zbus::zvariant::Value::I32(1)));
            if gained {
                return Some(Focused {
                    app: app.to_owned().into(),
                    path: path.to_owned().into(),
                });
            }
        }
        None
    }

    pub(super) async fn is_terminal(&self, focused: &Focused) -> bool {
        let role = async {
            AccessibleProxy::builder(&self.bus)
                .destination(focused.app.as_ref())?
                .path(focused.path.as_ref())?
                .build()
                .await?
                .get_role()
                .await
        }
        .await;
        log::debug!("focus moved to an accessible with role {role:?}");
        matches!(role, Ok(Role::Terminal))
    }
}

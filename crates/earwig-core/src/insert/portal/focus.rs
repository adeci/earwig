//! Whether a terminal has keyboard focus, from the accessibility bus
//! (AT-SPI). The portals tell clients nothing about the focused app, yet
//! terminals paste with Ctrl+Shift+V where everything else uses Ctrl+V.
//!
//! VTE terminals (GNOME Console, GNOME Terminal, Ptyxis) report the
//! `terminal` role. Terminals without accessibility support look like any
//! other app and get Ctrl+V.

use std::time::Duration;

use anyhow::Context as _;
use ashpd::zbus::proxy::CacheProperties;
use ashpd::zbus::{self, MatchRule, MessageStream, message};
use async_io::Timer;
use atspi_proxies::accessible::AccessibleProxy;
use atspi_proxies::bus::BusProxy;
use atspi_proxies::common::{ObjectRefOwned, Role, State, StateSet};
use atspi_proxies::registry::RegistryProxy;
use futures_lite::{StreamExt as _, future};

/// The registry's root accessible, whose children are the applications.
const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

/// Bounds for [`FocusWatch::current`]: a large or unresponsive app must not
/// hold up the watch.
const SEARCH_LIMIT: usize = 2000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// The accessible that has focus now. Focus events only report changes,
    /// so without this a terminal focused before the watch started (say,
    /// across a daemon restart) would get Ctrl+V until focus moves. Walks the
    /// showing part of the active windows, at most [`SEARCH_LIMIT`]
    /// accessibles and [`SEARCH_TIMEOUT`]. Qt reports focus on a widget and
    /// on whatever forwards focus to it (Konsole's terminal sits in a
    /// `filler` that does), so the deepest focused accessible wins.
    pub(super) async fn current(&self) -> Option<Focused> {
        let search = async {
            let mut stack = Vec::new();
            let apps = self.children(REGISTRY, ROOT).await.unwrap_or_default();
            for app in apps {
                let Some(name) = app.name() else { continue };
                for window in self
                    .children(name.as_str(), app.path_as_str())
                    .await
                    .unwrap_or_default()
                {
                    if self.state(&window).await.contains(State::Active) {
                        stack.push(window);
                    }
                }
            }
            let mut visited = 0;
            let mut found = None;
            while let Some(object) = stack.pop() {
                visited += 1;
                if visited > SEARCH_LIMIT {
                    break;
                }
                let Some(name) = object.name() else { continue };
                let state = self.state(&object).await;
                if state.contains(State::Focused) {
                    stack.clear();
                    found = Some(Focused {
                        app: name.to_owned().into(),
                        path: object.path().to_owned().into(),
                    });
                } else if !state.contains(State::Showing) {
                    continue;
                }
                let children = self.children(name.as_str(), object.path_as_str()).await;
                stack.extend(children.unwrap_or_default());
            }
            found
        };
        future::or(search, async {
            Timer::after(SEARCH_TIMEOUT).await;
            log::debug!("no focused accessible found within {SEARCH_TIMEOUT:?}");
            None
        })
        .await
    }

    pub(super) async fn is_terminal(&self, focused: &Focused) -> bool {
        let role = async {
            self.accessible(focused.app.as_str(), focused.path.as_str())
                .await?
                .get_role()
                .await
        }
        .await;
        log::debug!("focus moved to an accessible with role {role:?}");
        matches!(role, Ok(Role::Terminal))
    }

    async fn accessible<'a>(
        &self,
        app: &'a str,
        path: &'a str,
    ) -> zbus::Result<AccessibleProxy<'a>> {
        AccessibleProxy::builder(&self.bus)
            .destination(app)?
            .path(path)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
    }

    async fn children(&self, app: &str, path: &str) -> zbus::Result<Vec<ObjectRefOwned>> {
        self.accessible(app, path).await?.get_children().await
    }

    /// Empty when the accessible cannot be asked.
    async fn state(&self, object: &ObjectRefOwned) -> StateSet {
        let Some(name) = object.name() else {
            return StateSet::empty();
        };
        let state = async {
            self.accessible(name.as_str(), object.path_as_str())
                .await?
                .get_state()
                .await
        };
        state.await.unwrap_or_else(|_| StateSet::empty())
    }
}

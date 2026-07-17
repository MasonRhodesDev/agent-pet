//! Portable active-window source via `zwlr_foreign_toplevel_management_v1`.
//! Tracks which toplevel currently holds the `activated` state and reports
//! it as `ActiveWindow { pid: None, app_id, title }` (foreign-toplevel has
//! no pid). If the global is absent (GNOME, or a compositor without the
//! protocol) the source binds nothing and never emits — silent degrade.
//!
//! The manager runs on the renderer's own Wayland connection, so its events
//! are handled by `App`'s `Dispatch` impls (below); the resulting facts are
//! pushed into the same calloop channel the Hyprland thread uses.

use std::collections::HashMap;

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};
use tracing::{debug, info};

use crate::app::App;
use crate::compositor::{ActiveWindowSource, CompositorBackend, FactSink, SourceCtx};
use pet_proto::ActiveWindow;

/// wl_array of u32 state values; `activated` is value 2 (per the protocol).
const STATE_ACTIVATED: u32 = 2;

pub struct WlrGeneric;

/// Keeps the manager proxy alive for the process lifetime; dropping it stops
/// the compositor sending toplevel updates.
pub struct Source {
    _manager: ZwlrForeignToplevelManagerV1,
}

impl CompositorBackend for WlrGeneric {
    fn name(&self) -> &'static str {
        "wlr-generic"
    }

    fn start_active_window_source(&self, ctx: SourceCtx) -> ActiveWindowSource {
        // The sink is consumed via `App::wlr` (set by the caller before this
        // runs); Dispatch handlers can't see `SourceCtx`. Binding the manager
        // is all that is left to do here.
        match bind_manager(ctx.globals, ctx.qh) {
            Some(manager) => {
                info!("bound zwlr_foreign_toplevel_manager_v1");
                ActiveWindowSource::Wlr(Source { _manager: manager })
            }
            None => {
                debug!("no zwlr_foreign_toplevel_manager_v1; active-window source disabled");
                ActiveWindowSource::None
            }
        }
    }
}

fn bind_manager(
    globals: &GlobalList,
    qh: &QueueHandle<App>,
) -> Option<ZwlrForeignToplevelManagerV1> {
    globals.bind(qh, 1..=3, ()).ok()
}

/// Per-toplevel tracking, owned by `App`. Empty/inert unless the wlr backend
/// bound the manager.
#[derive(Default)]
pub struct Wlr {
    sink: Option<FactSink>,
    toplevels: HashMap<u32, Toplevel>,
    /// Last fact sent (dedup key — also covers a title change on the
    /// still-focused window).
    last_emitted: Option<Option<ActiveWindow>>,
}

#[derive(Default, Clone)]
struct Toplevel {
    app_id: Option<String>,
    title: Option<String>,
    /// Committed on the last `done`.
    activated: bool,
    // Staged between `done` events.
    pending_app_id: Option<String>,
    pending_title: Option<String>,
    pending_activated: bool,
}

impl Wlr {
    pub fn set_sink(&mut self, sink: FactSink) {
        self.sink = Some(sink);
    }

    fn window(t: &Toplevel) -> ActiveWindow {
        ActiveWindow {
            pid: None,
            address: None,
            app_id: t.app_id.clone(),
            title: t.title.clone(),
        }
    }

    /// Recompute the active toplevel after a `done`/`closed` and emit a fact
    /// when it — or the focused window's labels — changed. A title change on
    /// the still-focused window is a real change and is reported.
    fn reconcile(&mut self) {
        let fact = self.toplevels.values().find(|t| t.activated).map(Self::window);
        if self.last_emitted.as_ref() != Some(&fact) {
            self.last_emitted = Some(fact.clone());
            if let Some(sink) = &self.sink {
                let _ = sink.send(fact);
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(
        app: &mut App,
        _proxy: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => {
                app.wlr.toplevels.insert(toplevel.id().protocol_id(), Toplevel::default());
            }
            // The compositor stopped advertising the protocol.
            manager::Event::Finished => {
                app.wlr.toplevels.clear();
                app.wlr.reconcile();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(
        app: &mut App,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        let id = proxy.id().protocol_id();
        match event {
            handle::Event::Title { title } => {
                if let Some(t) = app.wlr.toplevels.get_mut(&id) {
                    t.pending_title = Some(title);
                }
            }
            handle::Event::AppId { app_id } => {
                if let Some(t) = app.wlr.toplevels.get_mut(&id) {
                    t.pending_app_id = Some(app_id);
                }
            }
            handle::Event::State { state } => {
                let activated = state
                    .chunks_exact(4)
                    .any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == STATE_ACTIVATED);
                if let Some(t) = app.wlr.toplevels.get_mut(&id) {
                    t.pending_activated = activated;
                }
            }
            handle::Event::Done => {
                if let Some(t) = app.wlr.toplevels.get_mut(&id) {
                    if let Some(a) = t.pending_app_id.take() {
                        t.app_id = Some(a);
                    }
                    if let Some(title) = t.pending_title.take() {
                        t.title = Some(title);
                    }
                    t.activated = t.pending_activated;
                }
                app.wlr.reconcile();
            }
            handle::Event::Closed => {
                app.wlr.toplevels.remove(&id);
                proxy.destroy();
                app.wlr.reconcile();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::channel;
    use smithay_client_toolkit::reexports::calloop::EventLoop;

    /// Drive `Wlr` directly (no Wayland) to check active-toplevel
    /// reconciliation and fact emission. The calloop channel is registered
    /// once and drained after each reconcile.
    #[test]
    fn reports_activated_toplevel_and_dedups() {
        let (tx, rx) = channel::channel::<Option<ActiveWindow>>();
        let mut evl = EventLoop::<Vec<Option<ActiveWindow>>>::try_new().unwrap();
        evl.handle()
            .insert_source(rx, |ev, _, acc: &mut Vec<Option<ActiveWindow>>| {
                if let channel::Event::Msg(m) = ev {
                    acc.push(m);
                }
            })
            .unwrap();
        let mut drain = |wlr: &mut Wlr| -> Vec<Option<ActiveWindow>> {
            wlr.reconcile();
            let mut out = Vec::new();
            evl.dispatch(std::time::Duration::from_millis(0), &mut out)
                .unwrap();
            out
        };

        let mut wlr = Wlr::default();
        wlr.set_sink(tx);
        wlr.toplevels.insert(
            1,
            Toplevel {
                app_id: Some("firefox".into()),
                ..Default::default()
            },
        );
        wlr.toplevels.insert(
            2,
            Toplevel {
                app_id: Some("kitty".into()),
                title: Some("claude".into()),
                activated: true,
                ..Default::default()
            },
        );

        let facts = drain(&mut wlr);
        assert_eq!(facts.len(), 1);
        let w = facts[0].clone().unwrap();
        assert_eq!(w.pid, None);
        assert_eq!(w.app_id.as_deref(), Some("kitty"));
        assert_eq!(w.title.as_deref(), Some("claude"));

        // No change -> no fact.
        assert!(drain(&mut wlr).is_empty());

        // Focus leaves all toplevels -> None fact.
        wlr.toplevels.get_mut(&2).unwrap().activated = false;
        assert_eq!(drain(&mut wlr), vec![None]);

        // A different toplevel activates -> its labels.
        wlr.toplevels.get_mut(&1).unwrap().activated = true;
        let facts = drain(&mut wlr);
        assert_eq!(facts[0].clone().unwrap().app_id.as_deref(), Some("firefox"));
    }
}

//! One renderer attempt: connect, build the mascot surface, then run a
//! calloop loop until the daemon shuts down (Ok) or something breaks (Err —
//! the supervisor in lib.rs recreates everything with backoff).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use pet_proto::{ActiveWindow, AgentState, Snapshot, UiAction};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop::channel;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle, RegistrationToken};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::PointerData;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::Shm;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::compose::{self, Geometry};
use crate::compositor::settle::{Action as SettleAction, Settle, DEFAULT_SETTLE_MS};
use crate::compositor::wlr_generic::Wlr;
use crate::compositor::{self, ActiveWindowSource, SourceCtx};
use crate::input::drag::{Drag, Walk};
use crate::input::router::{self, Clicks, Cursor, HoverChange, Rect};
use crate::sprite::pet_json::{resolve_pet_dir, PetDef};
use crate::sprite::semantics;
use crate::sprite::sheet::Sheet;
use crate::sprite::timeline::Timeline;
use crate::surface::bubble::AlertBubble;
use crate::surface::mascot::{Mascot, EDGE_MARGIN};
use crate::surface::position::Position;
use crate::surface::visibility::Visibility;
use crate::text::TextRenderer;
use crate::wayland::{buffers, globals};
use crate::{Control, ControlCmd};

pub struct App {
    pub registry_state: RegistryState,
    pub output_state: OutputState,
    pub seat_state: SeatState,
    pub compositor_state: CompositorState,
    pub shm: Shm,
    pub pool: SlotPool,
    pub mascot: Mascot,
    pub pet: PetDef,
    pub sheet: Sheet,
    pub timeline: Timeline,
    pub alert: AlertBubble,
    /// Lazy: font enumeration only happens once a bubble is needed.
    pub text: Option<TextRenderer>,
    pub position: Position,
    pub position_initialized: bool,
    pub position_path: PathBuf,
    pub drag: Drag,
    pub clicks: Clicks,
    /// Horizontal-travel tracker: drives the walk animation while dragging.
    pub walk: Option<Walk>,
    /// True while the pointer hovers the sprite and the jump gesture is up.
    pub hovering: bool,
    pub pointer: Option<WlPointer>,
    /// cursor-shape-v1: absent when the compositor lacks the global —
    /// degrade to no cursor changes. TODO(render-v1): wayland-cursor theme
    /// fallback for compositors without cursor-shape.
    pub cursor_shapes: Option<CursorShapeManager>,
    pub shape_device: Option<WpCursorShapeDeviceV1>,
    pub cursor: Cursor,
    pub qh: QueueHandle<App>,
    /// Render-thread clock epoch; the timeline runs in ms since this.
    pub started: Instant,
    pub last_state: AgentState,
    /// First-awake wave: fires once per daemon run on the reveal edge.
    pub greeted: bool,
    pub timer_token: Option<RegistrationToken>,
    pub loop_handle: LoopHandle<'static, App>,
    pub ui_tx: mpsc::UnboundedSender<UiAction>,
    pub last_control_seq: u64,
    /// Focus-fact debounce; the settled value is sent as
    /// UiAction::ActiveWindowChanged.
    pub settle: Settle,
    pub settle_token: Option<RegistrationToken>,
    /// wlr foreign-toplevel tracking (inert under Hyprland / when the global
    /// is absent).
    pub wlr: Wlr,
    /// Keeps the active-window source alive for the run's lifetime.
    pub active_window_source: ActiveWindowSource,
    pub shutdown: bool,
    pub error: Option<anyhow::Error>,
}

pub fn run(
    snapshot_rx: watch::Receiver<Arc<Snapshot>>,
    control_rx: watch::Receiver<Control>,
    ui_tx: mpsc::UnboundedSender<UiAction>,
) -> Result<()> {
    let pet_dir = resolve_pet_dir()?;
    let pet = PetDef::load(&pet_dir)?;
    let sheet = Sheet::load(&pet)?;
    info!(pet = %pet.id, dir = %pet_dir.display(), backend = ?compositor::detect(), "pet loaded");

    let position_path = Position::state_path();
    let saved = Position::load(&position_path);
    let position_initialized = saved.is_some();
    let position = saved.unwrap_or(Position {
        output_name: None,
        margin_x: 0,
        margin_y: 0,
        visible: true,
    });

    let conn = Connection::connect_to_env().context("connect to wayland display")?;
    let (globals_list, event_queue) = registry_queue_init::<App>(&conn).context("init registry")?;
    let qh = event_queue.handle();
    let bound = globals::bind(&globals_list, &qh)?;

    let sprite_scale = compose::sprite_scale_for(pet.frame_height);
    let mascot = Mascot::create(
        &bound.compositor,
        &bound.layer_shell,
        &qh,
        pet.frame_width * sprite_scale,
        pet.frame_height * sprite_scale,
        sprite_scale,
        position.visible,
    )?;
    let pool = SlotPool::new(
        (mascot.surf_w * mascot.surf_h * 4 * 2) as usize,
        &bound.shm,
    )
    .context("create shm pool")?;

    let mut event_loop = EventLoop::<'static, App>::try_new().context("create event loop")?;
    let handle = event_loop.handle();

    let started = Instant::now();
    let mut app = App {
        registry_state: bound.registry_state,
        output_state: bound.output_state,
        seat_state: bound.seat_state,
        compositor_state: bound.compositor,
        shm: bound.shm,
        pool,
        mascot,
        timeline: Timeline::new(&pet, 0),
        pet,
        sheet,
        alert: AlertBubble::default(),
        text: None,
        position,
        position_initialized,
        position_path,
        drag: Drag::Idle,
        clicks: Clicks::default(),
        walk: None,
        hovering: false,
        pointer: None,
        cursor_shapes: bound.cursor_shapes,
        shape_device: None,
        cursor: Cursor::Default,
        qh: qh.clone(),
        started,
        last_state: AgentState::Idle,
        greeted: false,
        timer_token: None,
        loop_handle: handle.clone(),
        ui_tx,
        last_control_seq: control_rx.borrow().seq,
        settle: Settle::new(DEFAULT_SETTLE_MS),
        settle_token: None,
        wlr: Wlr::default(),
        active_window_source: ActiveWindowSource::None,
        shutdown: false,
        error: None,
    };
    app.apply_snapshot(&snapshot_rx.borrow().clone());

    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .map_err(|e| anyhow!("insert wayland source: {e}"))?;

    let (snap_tx, snapshots) = channel::channel::<Arc<Snapshot>>();
    spawn_bridge("pet-render-bridge", snapshot_rx, snap_tx)?;
    handle
        .insert_source(snapshots, |event, _, app: &mut App| match event {
            channel::Event::Msg(snapshot) => {
                if app.apply_snapshot(&snapshot) && app.mascot.visibility.shown() {
                    app.render_frame();
                    app.rearm_timer();
                }
            }
            channel::Event::Closed => app.shutdown = true,
        })
        .map_err(|e| anyhow!("insert snapshot channel: {e}"))?;

    let (ctrl_tx, controls) = channel::channel::<Control>();
    spawn_bridge("pet-render-ctrl", control_rx, ctrl_tx)?;
    handle
        .insert_source(controls, |event, _, app: &mut App| match event {
            channel::Event::Msg(control) => app.apply_control(control),
            channel::Event::Closed => app.shutdown = true,
        })
        .map_err(|e| anyhow!("insert control channel: {e}"))?;

    // Active-window facts: the backend (Hyprland socket thread or wlr
    // foreign-toplevel on this connection) pushes raw facts here; the
    // renderer debounces (settle) before emitting ActiveWindowChanged. This
    // never blocks the dispatch — the Hyprland reader lives on its own thread.
    let backend = compositor::backend();
    info!(backend = backend.name(), "active-window source");
    let (fact_tx, facts) = channel::channel::<Option<ActiveWindow>>();
    app.wlr.set_sink(fact_tx.clone());
    app.active_window_source = backend.start_active_window_source(SourceCtx {
        globals: &globals_list,
        qh: &qh,
        sink: fact_tx,
    });
    handle
        .insert_source(facts, |event, _, app: &mut App| {
            if let channel::Event::Msg(window) = event {
                app.on_active_window_fact(window);
            }
        })
        .map_err(|e| anyhow!("insert active-window channel: {e}"))?;

    app.ensure_timer();

    loop {
        event_loop
            .dispatch(None::<Duration>, &mut app)
            .context("event loop dispatch")?;
        if let Some(error) = app.error.take() {
            return Err(error);
        }
        if app.shutdown {
            info!("daemon channels closed; renderer exiting");
            return Ok(());
        }
    }
}

/// The one frame timer: draw whatever is due, then sleep to the earliest
/// deadline (sprite frame or typewriter character). Parks itself while the
/// mascot is hidden; `ensure_timer` restarts it.
fn timer_tick(_: Instant, _: &mut (), app: &mut App) -> TimeoutAction {
    if !app.mascot.visibility.shown() {
        app.timer_token = None;
        return TimeoutAction::Drop;
    }
    app.render_frame();
    TimeoutAction::ToInstant(app.next_wakeup())
}

/// Forwards watch updates into a calloop channel. Exits when either side
/// goes away; dropping the sender tells the render loop to shut down.
fn spawn_bridge<T: Clone + Send + Sync + 'static>(
    name: &str,
    mut rx: watch::Receiver<T>,
    tx: channel::Sender<T>,
) -> Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || loop {
            if block_on(rx.changed()).is_err() {
                return; // daemon dropped the watch sender
            }
            let value = rx.borrow_and_update().clone();
            if tx.send(value).is_err() {
                return; // render loop went away
            }
        })
        .with_context(|| format!("spawn {name}"))?;
    Ok(())
}

impl App {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Returns true if anything visible changed (animation track or bubble).
    fn apply_snapshot(&mut self, snapshot: &Snapshot) -> bool {
        let mut changed = false;
        if snapshot.top != self.last_state {
            debug!(from = ?self.last_state, to = ?snapshot.top, "mascot state change");
            self.last_state = snapshot.top;
            // The drag walk (transientState) overrides the base state; only
            // drive the timeline when not dragging. The base track is
            // restored on release from `last_state`.
            if !self.drag.dragging() {
                let track = semantics::track_for(snapshot.top, &self.pet);
                self.timeline.request_state(track, self.now_ms());
            }
            changed = true;
        }
        // Reveal progress is keyed on content identity inside AlertBubble:
        // heartbeats, track switches, and transient clears never re-type.
        changed |= self.alert.apply(snapshot, self.now_ms());
        changed
    }

    fn apply_control(&mut self, control: Control) {
        if control.seq == self.last_control_seq {
            return; // stale replay (e.g. renderer restart)
        }
        self.last_control_seq = control.seq;
        match control.cmd {
            Some(ControlCmd::Show) => self.show(),
            Some(ControlCmd::Hide) => self.hide(),
            None => {}
        }
    }

    /// A raw active-window fact from the compositor backend. Runs it through
    /// the settle debounce and (re)arms the settle timer.
    fn on_active_window_fact(&mut self, window: Option<ActiveWindow>) {
        let action = self.settle.observe(window, Instant::now());
        self.apply_settle_action(action);
    }

    /// Settle timer fired: emit if the pending fact is due, else re-arm.
    fn on_settle_timer(&mut self) {
        self.settle_token = None;
        let action = self.settle.on_timer(Instant::now());
        self.apply_settle_action(action);
    }

    fn apply_settle_action(&mut self, action: SettleAction) {
        match action {
            SettleAction::None => {}
            SettleAction::ArmTimer(at) => {
                if let Some(token) = self.settle_token.take() {
                    self.loop_handle.remove(token);
                }
                match self
                    .loop_handle
                    .insert_source(Timer::from_deadline(at), |_, _, app: &mut App| {
                        app.on_settle_timer();
                        TimeoutAction::Drop
                    }) {
                    Ok(token) => self.settle_token = Some(token),
                    Err(e) => warn!("arm settle timer: {e}"),
                }
            }
            SettleAction::Emit(window) => {
                debug!(?window, "active window settled");
                let _ = self.ui_tx.send(UiAction::ActiveWindowChanged { window });
            }
        }
    }

    /// Whether a gesture/state track exists and has visible art. Gates the
    /// gesture animations so a pet with a blank row (the default pet leaves
    /// rows 1-4 transparent) never plays nothing.
    fn track_has_art(&self, track: &str) -> bool {
        self.pet.animations.get(track).is_some_and(|a| {
            self.sheet
                .any_visible(a.frames.iter().map(|f| f.sprite_index))
        })
    }

    /// Hover the sprite -> jump once (burst); leave -> back to the base
    /// state. Pure edge logic in `router::hover_transition`; only touches the
    /// animation, never the input routing (click/drag/right-click unaffected).
    pub(crate) fn set_hover(&mut self, over_sprite: bool) {
        let change = router::hover_transition(
            over_sprite,
            !self.drag.dragging(), // "docked" for hover purposes = not dragging
            self.drag.dragging(),
            self.track_has_art("jumping"),
            &mut self.hovering,
        );
        let track = match change {
            Some(HoverChange::Jump) => "jumping".to_string(),
            Some(HoverChange::ReturnToBase) => {
                semantics::track_for(self.last_state, &self.pet).to_string()
            }
            None => return,
        };
        self.timeline.request_state(&track, self.now_ms());
        self.render_frame();
        self.rearm_timer();
    }

    /// First-awake greeting: on the reveal edge, wave once (burst 3x ->
    /// idle). Skipped when something is already pending (the alert takes
    /// precedence) or when the pet has no waving art. One-time per run.
    pub(crate) fn maybe_greet(&mut self) {
        if self.greeted {
            return;
        }
        self.greeted = true;
        if self.last_state == AgentState::Idle && self.track_has_art("waving") {
            debug!("first-awake greeting wave");
            self.timeline.request_state("waving", self.now_ms());
        }
    }

    pub(crate) fn render_frame(&mut self) {
        if !self.mascot.configured || !self.mascot.visibility.shown() {
            return;
        }
        let now = self.now_ms();
        self.timeline.advance(now);

        // The pet is always a small docked surface; the sprite sits at the
        // layout-quadrant offset within it, and the surface itself moves (via
        // margins) to follow the cursor during a drag.
        let dragging = self.drag.dragging();
        if !dragging && self.alert.visible().is_some() && self.text.is_none() {
            self.text = Some(TextRenderer::new());
        }

        let (surf_w, surf_h) = self.mascot.surface_size();
        let (mascot_x, mascot_y) = (self.mascot.mascot_x, self.mascot.mascot_y);

        let geo = Geometry {
            surf_w,
            surf_h,
            mascot_x,
            mascot_y,
            mascot_w: self.mascot.mascot_w,
            bubble_above: self.mascot.bubble_above,
            anchor_right: self.mascot.anchor_right,
            sprite_scale: self.mascot.sprite_scale,
            oscale: self.mascot.output_scale.max(1) as u32,
        };
        let (buf_w, buf_h) = geo.buf_size();
        let oscale = geo.oscale as i32;
        let sheet = &mut self.sheet;
        let timeline = &self.timeline;
        // No bubble while dragging (repositioning, not reading); it returns
        // on drop when the docked surface is restored.
        let bubble = if dragging {
            None
        } else {
            self.alert.visible().zip(self.text.as_mut())
        };
        let result = buffers::present(
            &mut self.pool,
            self.mascot.layer.wl_surface(),
            buf_w,
            buf_h,
            |buf| compose::scene(buf, &geo, sheet, timeline, bubble, now),
        );
        let bubble_px = match result {
            Ok(px) => px,
            Err(e) => {
                self.error = Some(e.context("present frame"));
                return;
            }
        };
        // The drag surface's input region is the whole output (set once on
        // enter_drag); only the docked bubble box tracks per-frame.
        if dragging {
            return;
        }
        let rect = bubble_px.map(|(x, y, w, h)| Rect {
            x: x / oscale,
            y: y / oscale,
            w: w / oscale as u32,
            h: h / oscale as u32,
        });
        if rect != self.mascot.bubble_rect {
            self.mascot.bubble_rect = rect;
            if let Err(e) = self.mascot.update_input_region(&self.compositor_state) {
                warn!("input region update failed: {e:#}");
            }
            self.mascot.layer.commit();
        }
    }

    fn next_wakeup(&self) -> Instant {
        if !self.mascot.configured {
            // Poll gently until the first configure lands.
            return Instant::now() + Duration::from_millis(200);
        }
        let now = self.now_ms();
        let mut deadline = self.timeline.next_deadline_ms();
        if let Some(t) = self.alert.visible().and_then(|b| b.typing_deadline_ms(now)) {
            deadline = deadline.min(t);
        }
        let at = self.started + Duration::from_millis(deadline);
        at.max(Instant::now() + Duration::from_millis(1))
    }

    /// Start the frame timer if it is not running (post-show / startup).
    pub(crate) fn ensure_timer(&mut self) {
        if self.timer_token.is_some() || !self.mascot.visibility.shown() {
            return;
        }
        let handle = self.loop_handle.clone();
        match handle.insert_source(Timer::from_deadline(self.next_wakeup()), timer_tick) {
            Ok(token) => self.timer_token = Some(token),
            Err(e) => self.error = Some(anyhow!("arm frame timer: {e}")),
        }
    }

    /// Pull the running timer to a new (possibly earlier) deadline.
    fn rearm_timer(&mut self) {
        let handle = self.loop_handle.clone();
        if let Some(token) = self.timer_token.take() {
            handle.remove(token);
        }
        self.ensure_timer();
    }

    /// Apply a cursor shape (deduped). No-op without cursor-shape-v1.
    pub(crate) fn set_cursor(&mut self, cursor: Cursor) {
        if cursor == self.cursor {
            return;
        }
        self.cursor = cursor;
        let (Some(device), Some(pointer)) = (&self.shape_device, &self.pointer) else {
            return;
        };
        let Some(serial) = pointer
            .data::<PointerData>()
            .and_then(|d| d.latest_enter_serial())
        else {
            return;
        };
        let shape = match cursor {
            Cursor::Default => Shape::Default,
            Cursor::Pointer => Shape::Pointer,
            Cursor::Grab => Shape::Grab,
            Cursor::Grabbing => Shape::Grabbing,
        };
        device.set_shape(serial, shape);
    }

    pub(crate) fn set_output_scale(&mut self, factor: i32) {
        if factor == self.mascot.output_scale || factor < 1 {
            return;
        }
        self.mascot.output_scale = factor;
        if let Err(e) = self.mascot.layer.set_buffer_scale(factor as u32) {
            warn!("set_buffer_scale({factor}) unsupported: {e:?}");
            self.mascot.output_scale = 1;
            return;
        }
        self.render_frame();
    }

    /// Default bottom-right placement, once we know an output's geometry and
    /// nothing was persisted.
    pub(crate) fn ensure_position(&mut self) {
        if self.position_initialized {
            return;
        }
        let Some((out_w, out_h)) = self.output_logical() else {
            return;
        };
        self.position.margin_x = out_w - self.mascot.mascot_w as i32 - EDGE_MARGIN;
        self.position.margin_y = out_h - self.mascot.mascot_h as i32 - EDGE_MARGIN;
        self.position.output_name = self.output_name();
        self.position_initialized = true;
    }

    /// Recompute quadrant layout, margins, and input region from `position`.
    /// Skipped mid-drag: the drag moves margins directly and freezes the
    /// quadrant so the sprite offset doesn't jump.
    pub(crate) fn sync_layout(&mut self) {
        if self.drag.dragging() {
            return;
        }
        self.mascot.relayout(&self.position);
        if let Err(e) = self.mascot.update_input_region(&self.compositor_state) {
            warn!("input region update failed: {e:#}");
        }
        self.mascot.apply_margins(&self.position);
        if self.mascot.configured {
            self.mascot.layer.commit();
        }
    }

    /// Threshold crossed: start following the cursor. The mascot stays its
    /// small docked surface — Wayland's implicit pointer grab keeps motion
    /// events flowing even off the surface, so no expansion is needed and
    /// there is no coordinate-space transition to get wrong. The quadrant
    /// layout is frozen for the drag so the sprite offset within the surface
    /// stays fixed (only the margins move).
    pub(crate) fn begin_drag(&mut self) {
        self.hovering = false; // hover-jump is docked-only; clean slate
        // Only walk if the pet has directional walk art (both rows); the
        // default pet's rows 1-2 are blank, so it just slides.
        self.walk = (self.track_has_art("running-right") && self.track_has_art("running-left"))
            .then(|| Walk::new(self.position.margin_x));
    }

    /// A drag motion in the docked surface's local coords (which run off the
    /// surface bounds under the implicit grab). Moves the pet to keep the
    /// grab point under the cursor.
    pub(crate) fn on_drag_motion(&mut self, pointer: (f64, f64)) {
        let Some((mx, my)) = self.drag.drag_to(pointer) else {
            return;
        };
        self.position.margin_x = mx;
        self.position.margin_y = my;
        self.mascot.apply_margins(&self.position); // no relayout: quadrant frozen
        if let Some(walk) = self.walk.as_mut() {
            let flip = walk.update(mx);
            if let Some(dir) = flip {
                debug!(?dir, vel = walk.vel(), mx, "walk flip");
                self.timeline.request_loop(dir.track(), self.now_ms());
            }
        }
        // Render (draw the sprite + commit both the pending margin and buffer).
        self.render_frame();
    }

    /// Drag finished: clamp on-screen, re-pick the layout quadrant for the
    /// resting position, persist.
    pub(crate) fn drag_drop(&mut self) {
        let (ow, oh) = self
            .output_logical()
            .unwrap_or((self.mascot.surf_w as i32, self.mascot.surf_h as i32));
        self.position.clamp(
            ow,
            oh,
            self.mascot.mascot_w as i32,
            self.mascot.mascot_h as i32,
        );
        self.position.output_name = self.output_name();
        // Stop walking, return to the base state (kept fresh from snapshots).
        self.walk = None;
        let base = semantics::track_for(self.last_state, &self.pet);
        self.timeline.request_state(base, self.now_ms());
        self.mascot.relayout(&self.position);
        self.mascot.apply_margins(&self.position);
        if let Err(e) = self.mascot.update_input_region(&self.compositor_state) {
            warn!("input region update failed: {e:#}");
        }
        self.render_frame();
        self.position.save(&self.position_path);
        info!(x = self.position.margin_x, y = self.position.margin_y, "position saved");
    }

    pub(crate) fn hide(&mut self) {
        if self.mascot.visibility == Visibility::Hidden {
            return;
        }
        info!("hiding mascot");
        self.drag.release();
        self.mascot.unmap();
        self.position.visible = false;
        self.position.save(&self.position_path);
        // The frame timer parks itself on its next tick.
    }

    pub(crate) fn show(&mut self) {
        if self.mascot.visibility.shown() {
            return;
        }
        info!("showing mascot");
        self.position.visible = true;
        self.position.save(&self.position_path);
        if self.mascot.configured {
            // Never unmapped (startup-hidden): the first configure is still
            // valid, attach straight away.
            self.mascot.visibility = Visibility::Visible;
            self.maybe_greet();
            self.render_frame();
            self.ensure_timer();
        } else {
            self.mascot.request_remap(&self.position);
        }
    }

    fn output_logical(&self) -> Option<(i32, i32)> {
        let info = match &self.mascot.entered {
            Some(output) => self.output_state.info(output),
            None => self
                .output_state
                .outputs()
                .next()
                .and_then(|o| self.output_state.info(&o)),
        };
        info.as_ref().and_then(globals::logical_size)
    }

    fn output_name(&self) -> Option<String> {
        let info = match &self.mascot.entered {
            Some(output) => self.output_state.info(output),
            None => self
                .output_state
                .outputs()
                .next()
                .and_then(|o| self.output_state.info(&o)),
        };
        info.and_then(|i| i.name)
    }
}

/// Minimal executor for `watch::Receiver::changed()` — tokio sync primitives
/// don't need a runtime, only a waker.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::park(),
        }
    }
}


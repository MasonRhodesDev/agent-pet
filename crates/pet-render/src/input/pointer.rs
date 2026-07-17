//! Seat/pointer plumbing. Routing itself is pure (input/router.rs):
//! left-drag on the sprite moves the mascot, left-click on the bubble
//! focuses the alert's session, right-click hides. The compositor only
//! delivers events inside the input region (sprite + bubble box).

use pet_proto::UiAction;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{delegate_pointer, delegate_seat};
use tracing::{debug, info};

use crate::app::App;
use crate::input::drag::Release;
use crate::input::router::{cursor_for, hit_test, Cursor};
use crate::surface::mascot::SurfaceMode;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            if let (Some(pointer), Some(shapes)) = (&self.pointer, &self.cursor_shapes) {
                self.shape_device = Some(shapes.get_shape_device(pointer, qh));
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(device) = self.shape_device.take() {
                device.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if event.surface != *self.mascot.layer.wl_surface() {
                continue;
            }
            let hit = hit_test(
                event.position,
                self.mascot.sprite_rect(),
                self.mascot.bubble_rect,
            );
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.set_cursor(cursor_for(self.drag.dragging(), hit));
                }
                PointerEventKind::Leave { .. } => {
                    // A drag in progress survives Leave (the implicit grab
                    // keeps motion flowing); pending clicks do not. The
                    // cursor belongs to another surface now: reset state
                    // only, no protocol call.
                    if !self.drag.dragging() {
                        self.drag.release();
                    }
                    self.clicks.cancel();
                    self.cursor = Cursor::Default;
                }
                PointerEventKind::Motion { .. } => {
                    if self.mascot.mode == SurfaceMode::Drag {
                        // Stationary full-output surface: coords are output
                        // coords. Feed them straight to the absolute-offset
                        // drag (only once the resize has been acked, so we
                        // never mix the docked and full-output spaces).
                        if !self.mascot.resizing {
                            self.on_drag_motion(event.position);
                            self.set_cursor(Cursor::Grabbing);
                        }
                    } else if self.drag.threshold_crossed(event.position) {
                        // Docked surface: cross the click-vs-drag threshold ->
                        // expand to the full-output drag surface.
                        self.begin_drag();
                        self.set_cursor(Cursor::Grabbing);
                    } else {
                        self.set_cursor(cursor_for(false, hit));
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    if self.clicks.press(hit) {
                        self.drag.press(
                            event.position,
                            (self.position.margin_x, self.position.margin_y),
                        );
                    }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    if self.clicks.release(hit) {
                        if let Some(bubble) = self.alert.visible() {
                            info!(key = %bubble.key, "bubble clicked: focus session");
                            let _ = self.ui_tx.send(UiAction::FocusSession {
                                key: bubble.key.clone(),
                            });
                            // Optimistic: collapse the nag now rather than
                            // waiting on the focus-suppression round-trip
                            // (works for waiting alerts and focus-join misses
                            // too). The mascot track is unchanged.
                            if self.alert.dismiss_current() {
                                self.render_frame();
                            }
                        }
                    }
                    // Capture the final drag position before release clears it.
                    let final_position = self.drag.drag_pos();
                    match self.drag.release() {
                        Release::Dropped => self.drag_drop(final_position),
                        // Sprite click-without-drag: nothing (tray later).
                        Release::Click | Release::None => {}
                    }
                    self.set_cursor(cursor_for(false, hit));
                }
                PointerEventKind::Press { button: BTN_RIGHT, .. } => {
                    debug!("right-click: hiding mascot");
                    let _ = self.ui_tx.send(UiAction::SetVisible { visible: false });
                    self.drag.release();
                    self.clicks.cancel();
                    self.hide();
                }
                _ => {}
            }
        }
    }
}

delegate_seat!(App);
delegate_pointer!(App);

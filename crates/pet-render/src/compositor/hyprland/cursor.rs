//! Hyprland cursor-position source: a poll thread that queries socket1
//! `j/cursorpos` at ~15 Hz *only while gaze is wanted* and pushes each point
//! into the calloop channel. Wayland won't hand a layer surface the global
//! pointer, so this IPC poll is the only way to make the pet track the cursor
//! across the screen. It lives on its own thread — never touches the
//! renderer's Wayland connection or blocks the dispatch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::debug;

use crate::compositor::{CursorCtx, CursorSink};

use super::query;

/// ~15 Hz. Fast enough that the eyes track smoothly, slow enough that the
/// socket poll is negligible; only runs while `wanted` is true.
const POLL_INTERVAL: Duration = Duration::from_millis(66);
/// While gaze is idle, wake this often to notice `wanted` flipping on without
/// spinning a tight loop.
const IDLE_INTERVAL: Duration = Duration::from_millis(200);

/// Owns the poll thread; dropping it signals the thread to stop.
pub struct Source {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.handle.take());
    }
}

pub fn start(ctx: CursorCtx) -> Source {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let handle = std::thread::Builder::new()
        .name("pet-render-cursor".into())
        .spawn(move || poll_loop(&ctx.sink, &ctx.wanted, &thread_stop))
        .expect("spawn hyprland cursor thread");
    Source {
        stop,
        handle: Some(handle),
    }
}

fn poll_loop(sink: &CursorSink, wanted: &AtomicBool, stop: &AtomicBool) {
    let mut last: Option<(i32, i32)> = None;
    while !stop.load(Ordering::Relaxed) {
        if !wanted.load(Ordering::Relaxed) {
            last = None; // resend the first point when gaze resumes
            std::thread::sleep(IDLE_INTERVAL);
            continue;
        }
        match query::cursor_pos() {
            Ok(Some(pos)) => {
                // Only push movement — a still cursor needn't wake the loop.
                if last != Some(pos) {
                    last = Some(pos);
                    if sink.send(pos).is_err() {
                        return; // render loop went away
                    }
                }
            }
            Ok(None) => {}
            Err(e) => debug!("cursorpos query failed: {e:#}"),
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    debug!("hyprland cursor thread exiting");
}

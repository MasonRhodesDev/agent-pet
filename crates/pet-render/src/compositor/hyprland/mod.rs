//! Hyprland active-window source: a bridge thread reads the socket2 event
//! stream and, on each focus-relevant line, does a one-shot socket1
//! `j/activewindow` query and pushes the resulting fact into the calloop
//! channel. Auto-reconnects socket2 with backoff. Never touches the
//! renderer's Wayland connection or blocks the calloop dispatch.

pub mod cursor;
pub mod events;
pub mod query;

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::compositor::{
    ActiveWindowSource, CompositorBackend, CursorCtx, CursorSource, SourceCtx,
};

pub struct Hyprland;

/// Owns the reader thread; dropping it signals the thread to stop.
pub struct Source {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The reader may be parked in a blocking socket read; we do not join
        // (the process is exiting when a Source drops), just signal.
        drop(self.handle.take());
    }
}

impl CompositorBackend for Hyprland {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    fn start_active_window_source(&self, ctx: SourceCtx) -> ActiveWindowSource {
        let Some(path) = socket2_path() else {
            warn!("Hyprland signature/runtime dir missing; no active-window source");
            return ActiveWindowSource::None;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let sink = ctx.sink;
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("pet-render-hypr".into())
            .spawn(move || reader_loop(&path, &sink, &thread_stop))
            .expect("spawn hyprland reader thread");
        ActiveWindowSource::Hyprland(Source {
            stop,
            handle: Some(handle),
        })
    }

    fn start_cursor_source(&self, ctx: CursorCtx) -> CursorSource {
        CursorSource::Hyprland(cursor::start(ctx))
    }
}

const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
];

fn reader_loop(path: &str, sink: &crate::compositor::FactSink, stop: &AtomicBool) {
    let mut failures = 0usize;
    // Report the initial focus once so a window focused before we connected
    // suppresses correctly on startup.
    emit_current(sink);
    while !stop.load(Ordering::Relaxed) {
        match UnixStream::connect(path) {
            Ok(stream) => {
                failures = 0;
                info!("hyprland socket2 connected");
                // Re-query on (re)connect: focus may have moved while down.
                emit_current(sink);
                if read_events(stream, sink, stop).is_err() {
                    debug!("hyprland socket2 stream ended");
                }
            }
            Err(e) => debug!("hyprland socket2 connect failed: {e}"),
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let delay = BACKOFF[failures.min(BACKOFF.len() - 1)];
        failures += 1;
        std::thread::sleep(delay);
    }
    debug!("hyprland reader thread exiting");
}

/// Read newline-delimited socket2 events until the stream closes or stop is
/// set. Returns Err on I/O error so the loop reconnects.
fn read_events(
    stream: UnixStream,
    sink: &crate::compositor::FactSink,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    // A read timeout lets the thread notice `stop` between events instead of
    // blocking forever on an idle compositor.
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()), // EOF: compositor closed the socket
            Ok(_) => {
                let line = line.trim_end();
                if events::is_focus_cleared(line) {
                    let _ = sink.send(None);
                } else if events::is_focus_event(line) {
                    emit_current(sink);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue; // idle tick: re-check stop
            }
            Err(e) => return Err(e),
        }
    }
}

/// Query socket1 for the current active window and push the fact.
fn emit_current(sink: &crate::compositor::FactSink) {
    match query::active_window() {
        Ok(window) => {
            let _ = sink.send(window);
        }
        Err(e) => debug!("hyprland activewindow query failed: {e}"),
    }
}

fn socket2_path() -> Option<String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("{runtime_dir}/hypr/{signature}/.socket2.sock"))
}

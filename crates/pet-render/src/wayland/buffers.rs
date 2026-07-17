//! wl_shm presentation. SlotPool hands out a fresh slot while the previous
//! buffer is still held by the compositor, so this double-buffers naturally.

use anyhow::{Context as _, Result};
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::shm::slot::SlotPool;

/// Draw into a pool buffer (premultiplied ARGB8888; contents may be a stale
/// previous frame — clear first) and commit it. Passes the closure's value
/// back to the caller (e.g. the bubble rect for input-region sync).
pub fn present<T>(
    pool: &mut SlotPool,
    surface: &WlSurface,
    width: u32,
    height: u32,
    draw: impl FnOnce(&mut [u8]) -> T,
) -> Result<T> {
    let stride = width as i32 * 4;
    let (buffer, canvas) = pool
        .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
        .context("create shm buffer")?;
    let out = draw(canvas);
    buffer.attach_to(surface).context("attach shm buffer")?;
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
    Ok(out)
}

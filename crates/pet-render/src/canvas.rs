//! Software canvas over a premultiplied ARGB8888-LE (B,G,R,A) buffer —
//! just enough primitives for the mascot blit and the speech bubble.

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub f32);

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u8], width: u32, height: u32) -> Self {
        Self { buf, width, height }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    /// Copy a premultiplied frame onto a transparent area (no blending).
    pub fn blit(&mut self, src: &[u8], src_w: u32, src_h: u32, x: u32, y: u32) {
        if x + src_w > self.width || y + src_h > self.height {
            return;
        }
        let row_bytes = (src_w * 4) as usize;
        for sy in 0..src_h as usize {
            let d = (((y as usize + sy) * self.width as usize) + x as usize) * 4;
            let s = sy * row_bytes;
            self.buf[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
        }
    }

    /// Like `blit`, but mirror the source horizontally (reverse each row's
    /// pixels). Used for gaze frames whose left-facing directions reuse the
    /// right-facing art flipped.
    pub fn blit_flipped_h(&mut self, src: &[u8], src_w: u32, src_h: u32, x: u32, y: u32) {
        if x + src_w > self.width || y + src_h > self.height {
            return;
        }
        let sw = src_w as usize;
        let row_bytes = sw * 4;
        for sy in 0..src_h as usize {
            let d = (((y as usize + sy) * self.width as usize) + x as usize) * 4;
            let s = sy * row_bytes;
            for sx in 0..sw {
                let sp = s + (sw - 1 - sx) * 4;
                let dp = d + sx * 4;
                self.buf[dp..dp + 4].copy_from_slice(&src[sp..sp + 4]);
            }
        }
    }

    /// Source-over blend of a straight-alpha color at the given coverage.
    pub fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let a = (color.3 * coverage).clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = ((y as usize * self.width as usize) + x as usize) * 4;
        let over = |dst: u8, src: u8| -> u8 {
            (src as f32 * a + dst as f32 * (1.0 - a)).round() as u8
        };
        self.buf[i] = over(self.buf[i], color.2); // B
        self.buf[i + 1] = over(self.buf[i + 1], color.1); // G
        self.buf[i + 2] = over(self.buf[i + 2], color.0); // R
        self.buf[i + 3] = over(self.buf[i + 3], 255); // A
    }

    /// Anti-aliased rounded rectangle (signed-distance coverage).
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Rgba) {
        let r = radius.min(w / 2.0).min(h / 2.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        let x0 = (x.floor() as i32 - 1).max(0);
        let y0 = (y.floor() as i32 - 1).max(0);
        let x1 = ((x + w).ceil() as i32 + 1).min(self.width as i32);
        let y1 = ((y + h).ceil() as i32 + 1).min(self.height as i32);
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
                let dy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() - r;
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, color, cov);
                }
            }
        }
    }

    /// Isoceles triangle spanning `w`x`h` at (x, y); apex points down when
    /// `down`, up otherwise (the bubble's pointer toward the mascot).
    pub fn triangle(&mut self, x: f32, y: f32, w: f32, h: f32, down: bool, color: Rgba) {
        for row in 0..h.ceil() as i32 {
            let t = (row as f32 + 0.5) / h; // 0 at base edge
            let frac = if down { 1.0 - t } else { t };
            let half = (w / 2.0) * frac;
            let cx = x + w / 2.0;
            let (sx, ex) = (cx - half, cx + half);
            for px in sx.floor() as i32..=ex.ceil() as i32 {
                let fx = px as f32 + 0.5;
                let cov = ((ex - fx).min(fx - sx) + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, y as i32 + row, color, cov);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_copies_rows_and_respects_bounds() {
        let mut buf = vec![0u8; 4 * 4 * 4];
        let mut canvas = Canvas::new(&mut buf, 4, 4);
        let src = [7u8; 2 * 2 * 4];
        canvas.blit(&src, 2, 2, 1, 1);
        assert_eq!(&canvas.buf[(1 * 4 + 1) * 4..(1 * 4 + 3) * 4], &[7; 8]);
        assert_eq!(&canvas.buf[..4], &[0; 4]);
        // Out of bounds: no-op, no panic.
        canvas.blit(&src, 2, 2, 3, 3);
    }

    #[test]
    fn blend_is_source_over() {
        let mut buf = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut buf, 1, 1);
        canvas.blend(0, 0, Rgba(255, 0, 0, 1.0), 1.0);
        assert_eq!(canvas.buf, &[0, 0, 255, 255]);
        canvas.blend(0, 0, Rgba(0, 0, 255, 0.5), 1.0);
        assert_eq!(canvas.buf[0], 128); // half blue over red
        assert_eq!(canvas.buf[3], 255);
    }
}

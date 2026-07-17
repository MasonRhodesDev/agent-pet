//! Spritesheet decode: pre-sliced per-frame buffers in the exact layout
//! wl_shm wants (ARGB8888 = B,G,R,A bytes on little-endian, premultiplied).

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use super::pet_json::PetDef;

pub struct Sheet {
    pub frame_width: u32,
    pub frame_height: u32,
    /// Unscaled frames, row-major grid order (sprite_index order).
    base: Vec<Vec<u8>>,
    /// Per-frame visible vertical extent (top, bottom), unscaled pixels.
    vspans: Vec<(u32, u32)>,
    /// Nearest-neighbor integer upscales, keyed by factor.
    scaled: HashMap<u32, Vec<Vec<u8>>>,
}

impl Sheet {
    pub fn load(pet: &PetDef) -> Result<Self> {
        let img = image::open(&pet.spritesheet_path)
            .with_context(|| format!("decode {}", pet.spritesheet_path.display()))?
            .into_rgba8();
        Self::from_rgba(
            img.as_raw(),
            img.width(),
            img.height(),
            pet.frame_width,
            pet.frame_height,
        )
    }

    pub fn from_rgba(
        rgba: &[u8],
        sheet_w: u32,
        sheet_h: u32,
        frame_w: u32,
        frame_h: u32,
    ) -> Result<Self> {
        if frame_w == 0 || frame_h == 0 || sheet_w % frame_w != 0 || sheet_h % frame_h != 0 {
            bail!("frame grid {frame_w}x{frame_h} does not tile sheet {sheet_w}x{sheet_h}");
        }
        if rgba.len() != (sheet_w * sheet_h * 4) as usize {
            bail!("rgba buffer size does not match sheet dimensions");
        }
        let (cols, rows) = (sheet_w / frame_w, sheet_h / frame_h);
        let mut base = Vec::with_capacity((cols * rows) as usize);
        let mut vspans = Vec::with_capacity((cols * rows) as usize);
        for row in 0..rows {
            for col in 0..cols {
                let mut frame = Vec::with_capacity((frame_w * frame_h * 4) as usize);
                let (mut top, mut bottom) = (frame_h, 0);
                for y in 0..frame_h {
                    let sy = row * frame_h + y;
                    let start = ((sy * sheet_w + col * frame_w) * 4) as usize;
                    for px in rgba[start..start + (frame_w * 4) as usize].chunks_exact(4) {
                        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
                        frame.push(premul(b, a));
                        frame.push(premul(g, a));
                        frame.push(premul(r, a));
                        frame.push(a);
                        if a > 0 {
                            top = top.min(y);
                            bottom = bottom.max(y + 1);
                        }
                    }
                }
                base.push(frame);
                vspans.push((top, bottom));
            }
        }
        Ok(Self {
            frame_width: frame_w,
            frame_height: frame_h,
            base,
            vspans,
            scaled: HashMap::new(),
        })
    }

    /// Vertical extent of visible (alpha > 0) content across the given
    /// frames, in unscaled frame pixels: the anchor for hugging UI like the
    /// speech bubble, since sprites usually sit low in a padded frame.
    /// Falls back to the full frame when everything is transparent.
    pub fn content_vspan(&self, frames: impl IntoIterator<Item = usize>) -> (u32, u32) {
        let (mut top, mut bottom) = (self.frame_height, 0);
        for index in frames {
            if let Some(&(t, b)) = self.vspans.get(index) {
                top = top.min(t);
                bottom = bottom.max(b);
            }
        }
        if top >= bottom {
            (0, self.frame_height)
        } else {
            (top, bottom)
        }
    }

    pub fn frame_count(&self) -> usize {
        self.base.len()
    }

    /// Frames scaled by an integer factor (nearest-neighbor), cached.
    pub fn frames_at(&mut self, factor: u32) -> &[Vec<u8>] {
        let factor = factor.max(1);
        if factor == 1 {
            return &self.base;
        }
        self.scaled.entry(factor).or_insert_with(|| {
            self.base
                .iter()
                .map(|f| scale_frame(f, self.frame_width, self.frame_height, factor))
                .collect()
        })
    }
}

fn premul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

fn scale_frame(src: &[u8], w: u32, h: u32, factor: u32) -> Vec<u8> {
    let out_w = (w * factor) as usize;
    let mut out = Vec::with_capacity(out_w * (h * factor) as usize * 4);
    let mut row = Vec::with_capacity(out_w * 4);
    for y in 0..h as usize {
        row.clear();
        for x in 0..w as usize {
            let px = &src[(y * w as usize + x) * 4..][..4];
            for _ in 0..factor {
                row.extend_from_slice(px);
            }
        }
        for _ in 0..factor {
            out.extend_from_slice(&row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_premultiplies_and_scales() {
        // 2x1 sheet of 1x1 frames: half-transparent red, opaque green.
        let rgba = [255, 0, 0, 128, 0, 255, 0, 255];
        let mut sheet = Sheet::from_rgba(&rgba, 2, 1, 1, 1).unwrap();
        assert_eq!(sheet.frame_count(), 2);
        // Premultiplied BGRA: red*128/255 = 128 in the R slot (byte 2).
        assert_eq!(sheet.frames_at(1)[0], vec![0, 0, 128, 128]);
        assert_eq!(sheet.frames_at(1)[1], vec![0, 255, 0, 255]);
        // 2x nearest: 4 identical pixels.
        let scaled = &sheet.frames_at(2)[1];
        assert_eq!(scaled.len(), 16);
        assert_eq!(&scaled[..4], &[0, 255, 0, 255]);
        assert_eq!(&scaled[12..], &[0, 255, 0, 255]);
    }

    #[test]
    fn rejects_non_tiling_grid() {
        assert!(Sheet::from_rgba(&[0; 8], 2, 1, 3, 1).is_err());
    }

    #[test]
    fn content_vspan_tracks_visible_rows() {
        // One 1x4 frame: transparent, opaque, opaque, transparent.
        let rgba = [0, 0, 0, 0, 9, 9, 9, 255, 9, 9, 9, 255, 0, 0, 0, 0];
        let sheet = Sheet::from_rgba(&rgba, 1, 4, 1, 4).unwrap();
        assert_eq!(sheet.content_vspan([0]), (1, 3));
        // Fully transparent frames fall back to the whole frame.
        let clear = Sheet::from_rgba(&[0; 16], 1, 4, 1, 4).unwrap();
        assert_eq!(clear.content_vspan([0]), (0, 4));
        // Aggregation takes the union across frames (sheet is row-major:
        // each row holds one pixel of frame 0 then one of frame 1).
        let rgba2 = [
            0, 0, 0, 0, 0, 0, 0, 0, // y0: both clear
            9, 9, 9, 255, 0, 0, 0, 0, // y1: frame 0 ink
            0, 0, 0, 0, 9, 9, 9, 255, // y2: frame 1 ink
            0, 0, 0, 0, 0, 0, 0, 0, // y3: both clear
        ];
        let two = Sheet::from_rgba(&rgba2, 2, 4, 1, 4).unwrap();
        assert_eq!(two.content_vspan([0]), (1, 2));
        assert_eq!(two.content_vspan([0, 1]), (1, 3));
    }
}

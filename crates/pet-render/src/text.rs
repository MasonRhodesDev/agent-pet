//! cosmic-text wrapper: system-font text measured, truncated, and drawn as
//! alpha-blended glyphs onto the software canvas. All coordinates and sizes
//! here are physical pixels (callers multiply by the buffer scale).

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};

use crate::canvas::{Canvas, Rgba};

pub struct TextRenderer {
    font_system: FontSystem,
    cache: SwashCache,
}

#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub font_px: f32,
    pub line_px: f32,
}

impl TextRenderer {
    /// Loads system fonts via fontdb — do this lazily, it takes a moment.
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    fn shape(&mut self, text: &str, style: TextStyle, wrap_w: f32) -> Buffer {
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(style.font_px, style.line_px),
        );
        buffer.set_size(&mut self.font_system, Some(wrap_w), None);
        buffer.set_text(
            &mut self.font_system,
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    /// (widest line, line count) after wrapping.
    pub fn measure(&mut self, text: &str, style: TextStyle, wrap_w: f32) -> (f32, usize) {
        let buffer = self.shape(text, style, wrap_w);
        let mut width: f32 = 0.0;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        (width, lines)
    }

    /// Cut `text` to at most `max_lines` wrapped lines, appending an
    /// ellipsis when something was dropped.
    pub fn truncate_lines(
        &mut self,
        text: &str,
        style: TextStyle,
        wrap_w: f32,
        max_lines: usize,
    ) -> String {
        let buffer = self.shape(text, style, wrap_w);
        let line_starts = line_starts(text);
        let mut end_byte = None;
        for (i, run) in buffer.layout_runs().enumerate() {
            if i + 1 == max_lines {
                let run_end = run.glyphs.last().map(|g| g.end).unwrap_or(0);
                end_byte = Some(line_starts[run.line_i] + run_end);
            } else if i >= max_lines {
                // There is a dropped line: truncate at the recorded end.
                let mut cut = text[..end_byte.unwrap_or(0)].trim_end().to_string();
                cut.pop();
                cut.push('…');
                return cut;
            }
        }
        text.to_string()
    }

    /// Draw at (x, y), revealing only glyphs starting before `max_bytes`
    /// (byte offset into `text`; `None` = all).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: TextStyle,
        wrap_w: f32,
        x: i32,
        y: i32,
        color: Rgba,
        max_bytes: Option<usize>,
    ) {
        let buffer = self.shape(text, style, wrap_w);
        let line_starts = line_starts(text);
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                if let Some(limit) = max_bytes {
                    if line_starts[run.line_i] + glyph.start >= limit {
                        continue;
                    }
                }
                // The (x, y) offset is baked into `physical` here — do not
                // add it again below (it doubles the offset and scatters
                // lines outside the bubble).
                let physical = glyph.physical((x as f32, y as f32), 1.0);
                let Some(image) = self
                    .cache
                    .get_image(&mut self.font_system, physical.cache_key)
                else {
                    continue;
                };
                let gx = physical.x + image.placement.left;
                let gy = run.line_y as i32 + physical.y - image.placement.top;
                let (w, h) = (image.placement.width as i32, image.placement.height as i32);
                match image.content {
                    cosmic_text::SwashContent::Mask => {
                        for py in 0..h {
                            for px in 0..w {
                                let alpha = image.data[(py * w + px) as usize] as f32 / 255.0;
                                canvas.blend(gx + px, gy + py, color, alpha);
                            }
                        }
                    }
                    cosmic_text::SwashContent::Color => {
                        for py in 0..h {
                            for px in 0..w {
                                let i = ((py * w + px) * 4) as usize;
                                let &[r, g, b, a] = &image.data[i..i + 4] else {
                                    continue;
                                };
                                canvas.blend(
                                    gx + px,
                                    gy + py,
                                    Rgba(r, g, b, a as f32 / 255.0),
                                    1.0,
                                );
                            }
                        }
                    }
                    cosmic_text::SwashContent::SubpixelMask => {
                        for py in 0..h {
                            for px in 0..w {
                                let i = ((py * w + px) * 3) as usize;
                                let alpha = image.data[i] as f32 / 255.0;
                                canvas.blend(gx + px, gy + py, color, alpha);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Byte offset of each `\n`-separated line, indexed by cosmic-text's
/// buffer-line index (glyph byte offsets are line-relative).
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_maps_buffer_lines_to_byte_offsets() {
        assert_eq!(line_starts("ab\ncd\n\ne"), vec![0, 3, 6, 7]);
        assert_eq!(line_starts("abc"), vec![0]);
    }
}

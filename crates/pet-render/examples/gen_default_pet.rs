//! Generates the committed default pet spritesheet:
//!
//!   cargo run -p pet-render --example gen_default_pet -- assets/default-pet
//!
//! 8 columns x 9 rows of 64x64 frames (512x576). Semantic rows follow the
//! Codex layout: 0 idle, 5 failed, 6 waiting, 7 running, 8 review; rows 1-4
//! stay transparent (Codex uses them for move/wave/bounce, unused here).

use image::{Rgba, RgbaImage};

const FRAME: u32 = 64;
const COLS: u32 = 8;
const ROWS: u32 = 9;

#[derive(Clone, Copy)]
struct Look {
    body: [u8; 3],
    /// Vertical body offset (positive = down).
    bob: f32,
    /// Horizontal sway.
    sway: f32,
    /// Height multiplier; width compensates to keep apparent volume.
    squash: f32,
    eyes: Eyes,
    /// Alpha of the "!" mark above the head (0 = none).
    bang: f32,
    smile: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Eyes {
    Open,
    Blink,
    Sad,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            body: [143, 163, 200], // periwinkle
            bob: 0.0,
            sway: 0.0,
            squash: 1.0,
            eyes: Eyes::Open,
            bang: 0.0,
            smile: false,
        }
    }
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/default-pet".to_string());
    let mut sheet = RgbaImage::new(FRAME * COLS, FRAME * ROWS);

    for (col, look) in idle_frames().into_iter().enumerate() {
        draw_frame(&mut sheet, col as u32, 0, look);
    }
    for (col, look) in failed_frames().into_iter().enumerate() {
        draw_frame(&mut sheet, col as u32, 5, look);
    }
    for (col, look) in waiting_frames().into_iter().enumerate() {
        draw_frame(&mut sheet, col as u32, 6, look);
    }
    for (col, look) in running_frames().into_iter().enumerate() {
        draw_frame(&mut sheet, col as u32, 7, look);
    }
    for (col, look) in review_frames().into_iter().enumerate() {
        draw_frame(&mut sheet, col as u32, 8, look);
    }

    std::fs::create_dir_all(&out_dir).expect("create output dir");
    let path = format!("{out_dir}/spritesheet.png");
    sheet.save(&path).expect("write spritesheet");
    println!("wrote {path}");
}

fn idle_frames() -> Vec<Look> {
    // Paced by pet.json's Codex idle timings: rest, up, blink, down, up, rest.
    [(0.0, Eyes::Open), (-1.0, Eyes::Open), (-1.0, Eyes::Blink), (1.0, Eyes::Open), (-0.5, Eyes::Open), (0.0, Eyes::Open)]
        .into_iter()
        .map(|(bob, eyes)| Look { bob, eyes, ..Default::default() })
        .collect()
}

fn failed_frames() -> Vec<Look> {
    // Red, deflating droop that settles low.
    [1.0, 0.96, 0.92, 0.87, 0.84, 0.82, 0.84, 0.87]
        .into_iter()
        .map(|squash| Look {
            body: [199, 84, 80],
            squash,
            bob: (1.0 - squash) * 14.0,
            eyes: Eyes::Sad,
            ..Default::default()
        })
        .collect()
}

fn waiting_frames() -> Vec<Look> {
    // Amber, attentive, exclamation mark pulsing overhead.
    [(1.0, -1.0), (0.75, -2.0), (0.45, -1.0), (0.45, 0.0), (0.75, -1.0), (1.0, -2.0)]
        .into_iter()
        .map(|(bang, bob)| Look {
            body: [217, 164, 65],
            bang,
            bob,
            ..Default::default()
        })
        .collect()
}

fn running_frames() -> Vec<Look> {
    // Teal, busy bounce with squash-and-stretch on landing.
    [(0.0, 1.0), (-4.0, 1.06), (-6.0, 1.1), (-3.0, 1.04), (0.0, 0.98), (1.5, 0.9)]
        .into_iter()
        .map(|(bob, squash)| Look {
            body: [59, 169, 156],
            bob,
            squash,
            ..Default::default()
        })
        .collect()
}

fn review_frames() -> Vec<Look> {
    // Green, satisfied side-to-side wiggle with a smile.
    [(-2.5, 0.0), (-1.5, -1.0), (0.0, -2.0), (1.5, -1.0), (2.5, 0.0), (0.0, 0.0)]
        .into_iter()
        .map(|(sway, bob)| Look {
            body: [88, 166, 92],
            sway,
            bob,
            smile: true,
            ..Default::default()
        })
        .collect()
}

fn draw_frame(sheet: &mut RgbaImage, col: u32, row: u32, look: Look) {
    let (ox, oy) = (col * FRAME, row * FRAME);
    let cx = 32.0 + look.sway;
    // Keep the body's floor fixed while squashing.
    let ry = 15.0 * look.squash;
    let rx = 19.0 / look.squash.sqrt();
    let cy = 52.0 - ry + look.bob;

    let outline = darken(look.body, 0.55);
    let highlight = lighten(look.body, 0.45);
    for y in 0..FRAME {
        for x in 0..FRAME {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut px: Option<([u8; 3], f32)> = None;

            let body_d = ellipse_dist(fx, fy, cx, cy, rx, ry);
            let out_a = coverage(body_d, rx.min(ry) + 1.5);
            let in_a = coverage(body_d, rx.min(ry) - 0.2);
            if out_a > 0.0 {
                let color = mix(outline, look.body, in_a);
                px = Some((color, out_a));
            }
            // Soft top-left sheen.
            let hl = coverage(
                ellipse_dist(fx, fy, cx - rx * 0.35, cy - ry * 0.45, rx * 0.3, ry * 0.25),
                3.0,
            );
            if hl > 0.0 {
                let (base, a) = px.unwrap_or((look.body, 0.0));
                px = Some((mix(base, highlight, hl * 0.7), a.max(hl * out_a)));
            }

            for (part_color, alpha) in features(fx, fy, cx, cy, ry, look) {
                let (base, a) = px.unwrap_or(([0, 0, 0], 0.0));
                px = Some((mix(base, part_color, alpha), a.max(alpha)));
            }

            if let Some((color, alpha)) = px {
                if alpha > 0.003 {
                    sheet.put_pixel(
                        ox + x,
                        oy + y,
                        Rgba([color[0], color[1], color[2], (alpha * 255.0) as u8]),
                    );
                }
            }
        }
    }
}

/// Eyes, mouth, exclamation mark: (color, coverage) contributions at a pixel.
fn features(fx: f32, fy: f32, cx: f32, cy: f32, ry: f32, look: Look) -> Vec<([u8; 3], f32)> {
    let ink = [34, 36, 46];
    let mut parts = Vec::new();
    let eye_y = cy - ry * 0.25;
    for side in [-1.0f32, 1.0] {
        let ex = cx + side * 7.5;
        let a = match look.eyes {
            Eyes::Open => coverage(ellipse_dist(fx, fy, ex, eye_y, 2.4, 3.0), 2.4),
            Eyes::Blink => coverage(ellipse_dist(fx, fy, ex, eye_y + 1.0, 2.6, 0.8), 1.2),
            Eyes::Sad => coverage(ellipse_dist(fx, fy, ex, eye_y + 2.0, 2.6, 0.9), 1.2),
        };
        if a > 0.0 {
            parts.push((ink, a));
        }
    }
    if look.smile {
        // Shallow arc under the eyes.
        let d = ellipse_dist(fx, fy, cx, cy + ry * 0.15, 5.5, 3.6);
        let band = (1.0 - (d - 0.75).abs() * 4.0).clamp(0.0, 1.0);
        let lower = ((fy - (cy + ry * 0.18)) / 3.0).clamp(0.0, 1.0);
        parts.push((ink, band * lower));
    }
    if look.bang > 0.0 {
        let top = cy - ry - 16.0;
        let bar = coverage(ellipse_dist(fx, fy, cx, top + 4.0, 1.6, 4.5), 1.8);
        let dot = coverage(ellipse_dist(fx, fy, cx, top + 12.5, 1.6, 1.6), 1.6);
        let a = (bar + dot).min(1.0) * look.bang;
        if a > 0.0 {
            parts.push(([236, 200, 90], a));
        }
    }
    parts
}

/// Normalized ellipse distance: 1.0 on the boundary.
fn ellipse_dist(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
    (dx * dx + dy * dy).sqrt()
}

/// Anti-aliased coverage for a normalized distance, ~1px soft edge for a
/// shape of radius `r`.
fn coverage(dist: f32, r: f32) -> f32 {
    ((1.0 - dist) * r.max(0.5)).clamp(0.0, 1.0)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

fn darken(c: [u8; 3], f: f32) -> [u8; 3] {
    [(c[0] as f32 * f) as u8, (c[1] as f32 * f) as u8, (c[2] as f32 * f) as u8]
}

fn lighten(c: [u8; 3], f: f32) -> [u8; 3] {
    mix(c, [255, 255, 255], f)
}

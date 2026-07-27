//! Pure multi-output geometry. Outputs are logical rects in the compositor's
//! global coordinate space; these queries let the mascot place itself across
//! any monitor and recover gracefully when one goes away.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRect {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl OutputRect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// The output whose rect contains the point, if any.
pub fn output_at<'a>(outputs: &'a [OutputRect], px: i32, py: i32) -> Option<&'a OutputRect> {
    outputs.iter().find(|o| o.contains(px, py))
}

/// The output whose centre is nearest the point. The fallback when no output
/// contains it — e.g. the pet's monitor was unplugged and its global position
/// is now off every screen, so it slides to the closest remaining one.
pub fn nearest_output<'a>(outputs: &'a [OutputRect], px: i32, py: i32) -> Option<&'a OutputRect> {
    outputs.iter().min_by_key(|o| {
        let (cx, cy) = o.center();
        let (dx, dy) = ((px - cx) as i64, (py - cy) as i64);
        dx * dx + dy * dy
    })
}

/// Clamp a mascot's global top-left so a `w×h` sprite stays fully inside
/// `rect`. Degenerate outputs (smaller than the sprite) pin to the origin.
pub fn clamp_into(rect: &OutputRect, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let max_x = (rect.x + rect.w - w).max(rect.x);
    let max_y = (rect.y + rect.h - h).max(rect.y);
    (x.clamp(rect.x, max_x), y.clamp(rect.y, max_y))
}

/// Resolve a global point to the output that should host the pet and the
/// output-local top-left within it: the containing output, else the nearest.
/// This is the bridge the surface layer uses — persist/drag in global coords,
/// render in per-output local margins.
pub fn resolve<'a>(
    outputs: &'a [OutputRect],
    gx: i32,
    gy: i32,
) -> Option<(&'a OutputRect, i32, i32)> {
    let out = output_at(outputs, gx, gy).or_else(|| nearest_output(outputs, gx, gy))?;
    Some((out, gx - out.x, gy - out.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) -> OutputRect {
        OutputRect {
            name: name.into(),
            x,
            y,
            w,
            h,
        }
    }

    fn two_side_by_side() -> Vec<OutputRect> {
        // DP-1 at origin (3440×1440), DP-2 to its right (1920×1080).
        vec![
            rect("DP-1", 0, 0, 3440, 1440),
            rect("DP-2", 3440, 0, 1920, 1080),
        ]
    }

    #[test]
    fn output_at_picks_the_containing_monitor() {
        let outs = two_side_by_side();
        assert_eq!(
            output_at(&outs, 100, 100).map(|o| o.name.as_str()),
            Some("DP-1")
        );
        assert_eq!(
            output_at(&outs, 3500, 200).map(|o| o.name.as_str()),
            Some("DP-2")
        );
        // The right edge of DP-1 (x=3440) belongs to DP-2, not DP-1 (half-open).
        assert_eq!(
            output_at(&outs, 3440, 0).map(|o| o.name.as_str()),
            Some("DP-2")
        );
        assert_eq!(
            output_at(&outs, 3439, 0).map(|o| o.name.as_str()),
            Some("DP-1")
        );
        // Below DP-2 (y>1080) but within DP-1's height gap: off every screen.
        assert_eq!(output_at(&outs, 4000, 1200), None);
    }

    #[test]
    fn nearest_output_is_the_off_screen_fallback() {
        let outs = two_side_by_side();
        // A point off the bottom-right: nearest DP-2 (its centre is closer).
        assert_eq!(
            nearest_output(&outs, 5400, 1400).map(|o| o.name.as_str()),
            Some("DP-2")
        );
        // Far left of everything → DP-1.
        assert_eq!(
            nearest_output(&outs, -500, 700).map(|o| o.name.as_str()),
            Some("DP-1")
        );
        assert_eq!(nearest_output(&[], 0, 0), None);
    }

    #[test]
    fn clamp_into_keeps_the_sprite_fully_on_screen() {
        let r = rect("DP-2", 3440, 0, 1920, 1080);
        let (w, h) = (192, 208);
        // Past the right/bottom edges → pulled back so the sprite fits.
        assert_eq!(
            clamp_into(&r, 9999, 9999, w, h),
            (3440 + 1920 - 192, 1080 - 208)
        );
        // Before the origin → snapped to the origin.
        assert_eq!(clamp_into(&r, 3000, -50, w, h), (3440, 0));
        // Already inside → unchanged.
        assert_eq!(clamp_into(&r, 3500, 100, w, h), (3500, 100));
    }

    #[test]
    fn resolve_maps_global_to_output_local() {
        let outs = two_side_by_side();
        // A point on DP-2 → DP-2 + local coords relative to its origin.
        let (o, lx, ly) = resolve(&outs, 3600, 300).unwrap();
        assert_eq!((o.name.as_str(), lx, ly), ("DP-2", 160, 300));
        // A point on DP-1.
        let (o, lx, ly) = resolve(&outs, 785, 767).unwrap();
        assert_eq!((o.name.as_str(), lx, ly), ("DP-1", 785, 767));
        // Off every screen → nearest output, local coords may be out of its
        // bounds (caller clamps).
        let (o, _, _) = resolve(&outs, 6000, 2000).unwrap();
        assert_eq!(o.name.as_str(), "DP-2");
        assert!(resolve(&[], 0, 0).is_none());
    }

    #[test]
    fn degenerate_output_smaller_than_sprite_pins_to_origin() {
        let r = rect("tiny", 0, 0, 100, 100);
        assert_eq!(clamp_into(&r, 500, 500, 192, 208), (0, 0));
    }
}

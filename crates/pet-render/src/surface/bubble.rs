//! Alert speech bubble: what to say (from the snapshot), the typewriter
//! reveal, and how to paint it into the bubble zone of the surface canvas.

use pet_proto::{AgentState, SessionKey, Snapshot};

use crate::canvas::{Canvas, Rgba};
use crate::text::{TextRenderer, TextStyle};

/// Logical geometry (multiplied by the buffer scale when drawing).
pub const MAX_WIDTH: u32 = 280;
pub const PAD: u32 = 10;
pub const LINE_PX: u32 = 18;
pub const FONT_PX: f32 = 14.0;
pub const LABEL_FONT_PX: f32 = 12.0;
pub const TRI_W: u32 = 14;
/// Gap between the bubble's box edge and the sprite's *visible* content;
/// the tail triangle spans it, its tip stopping 1px shy of the sprite.
pub const BOX_GAP: u32 = 8;
pub const MAX_BODY_LINES: usize = 3;
pub const MS_PER_CHAR: u64 = 30;

const BG: Rgba = Rgba(24, 26, 32, 0.85);
const LABEL_COLOR: Rgba = Rgba(255, 214, 130, 1.0);
const BODY_COLOR: Rgba = Rgba(233, 235, 240, 1.0);

/// Fixed logical height reserved above/below the mascot for the bubble
/// (label line + max body lines + padding + sprite gap).
pub const fn zone_height() -> u32 {
    PAD * 2 + LINE_PX * (1 + MAX_BODY_LINES as u32) + BOX_GAP
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bubble {
    pub key: SessionKey,
    pub label: &'static str,
    pub body: String,
    started_ms: u64,
    chars_total: usize,
}

impl Bubble {
    pub fn new(key: SessionKey, label: &'static str, body: String, now_ms: u64) -> Self {
        let chars_total = body.chars().count();
        Self {
            key,
            label,
            body,
            started_ms: now_ms,
            chars_total,
        }
    }

    pub fn visible_chars(&self, now_ms: u64) -> usize {
        ((now_ms.saturating_sub(self.started_ms) / MS_PER_CHAR) as usize).min(self.chars_total)
    }

    /// Byte offset into `body` for the reveal cutoff.
    pub fn visible_bytes(&self, now_ms: u64) -> usize {
        let chars = self.visible_chars(now_ms);
        self.body
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.body.len())
    }

    /// When the next character appears; None once fully revealed.
    pub fn typing_deadline_ms(&self, now_ms: u64) -> Option<u64> {
        let visible = self.visible_chars(now_ms);
        if visible >= self.chars_total {
            return None;
        }
        Some(self.started_ms + (visible as u64 + 1) * MS_PER_CHAR)
    }
}

/// Alert bubble state: what is shown, plus a reveal memory keyed purely on
/// the content identity (session key, label, body). Anything that leaves
/// that triple unchanged — animation track switches, snapshot heartbeats,
/// seen-count noise, transient alert clears, hide/show round-trips — keeps
/// the typewriter's progress; a changed triple re-types from zero.
#[derive(Debug, Default)]
pub struct AlertBubble {
    shown: bool,
    /// Most recent reveal (content + start time), kept while hidden.
    slot: Option<Bubble>,
}

impl AlertBubble {
    /// Apply a snapshot. Returns true when the *visible* bubble changed
    /// (appeared, disappeared, or switched content) and a redraw is due.
    pub fn apply(&mut self, snapshot: &Snapshot, now_ms: u64) -> bool {
        match alert_for(snapshot) {
            Some((key, label, body)) => {
                let same_content = self.slot.as_ref().is_some_and(|b| {
                    b.key == key && b.label == label && b.body == body
                });
                if !same_content {
                    self.slot = Some(Bubble::new(key, label, body.to_string(), now_ms));
                }
                let changed = !self.shown || !same_content;
                self.shown = true;
                changed
            }
            None => {
                let changed = self.shown;
                self.shown = false;
                changed
            }
        }
    }

    pub fn visible(&self) -> Option<&Bubble> {
        if self.shown {
            self.slot.as_ref()
        } else {
            None
        }
    }
}

/// What the bubble should say for this snapshot: alert states only
/// (needs-input / blocked / unseen-ready — `top` is already the priority
/// reduce), and only when the top session carries a body.
pub fn alert_for(snapshot: &Snapshot) -> Option<(SessionKey, &'static str, &str)> {
    if !matches!(
        snapshot.top,
        AgentState::Waiting | AgentState::Failed | AgentState::Ready
    ) {
        return None;
    }
    let session = snapshot.sessions.iter().find(|s| s.state == snapshot.top)?;
    let body = session.body.as_deref().filter(|b| !b.trim().is_empty())?;
    Some((session.key.clone(), snapshot.top.label(), body))
}

/// Where the bubble anchors, in physical pixels. The anchor is the SPRITE'S
/// VISIBLE CONTENT (alpha extent), never the canvas edges — sprites sit low
/// inside padded frames, and the bubble must hug what the eye sees.
pub struct BubbleArea {
    pub canvas_w: u32,
    pub canvas_h: u32,
    /// Sprite rect (x, w) — horizontal alignment reference.
    pub sprite_x: i32,
    pub sprite_w: u32,
    /// Physical y of the sprite's visible content top / bottom edges.
    pub content_top: i32,
    pub content_bottom: i32,
    /// Bubble sits above the sprite (pointer at the bubble's bottom).
    pub above: bool,
    /// Sprite is in the right half: wide bubbles align right edges.
    pub anchor_right: bool,
    pub scale: u32,
}

/// Paints the bubble and returns the box rect (physical px, excluding the
/// tail) — the click target and input-region contribution.
pub fn draw(
    bubble: &Bubble,
    text: &mut TextRenderer,
    canvas: &mut Canvas,
    area: &BubbleArea,
    now_ms: u64,
) -> (i32, i32, u32, u32) {
    let f = area.scale as f32;
    let pad = PAD as f32 * f;
    let line = LINE_PX as f32 * f;
    let label_style = TextStyle {
        font_px: LABEL_FONT_PX * f,
        line_px: line,
    };
    let body_style = TextStyle {
        font_px: FONT_PX * f,
        line_px: line,
    };
    let max_w = (MAX_WIDTH * area.scale).min(area.canvas_w) as f32;
    let wrap_w = max_w - 2.0 * pad;

    let body = text.truncate_lines(&bubble.body, body_style, wrap_w, MAX_BODY_LINES);
    let (label_w, _) = text.measure(bubble.label, label_style, wrap_w);
    let (body_w, body_lines) = text.measure(&body, body_style, wrap_w);

    let box_w = (label_w.max(body_w) + 2.0 * pad).min(max_w);
    let box_h = 2.0 * pad + line * (1 + body_lines) as f32;
    let gap = BOX_GAP as f32 * f;

    // Horizontal: centered on the sprite when it fits, else edge-aligned
    // with the sprite's near edge; always clamped onto the canvas.
    let sprite_cx = area.sprite_x as f32 + area.sprite_w as f32 / 2.0;
    let bx = if box_w <= area.sprite_w as f32 {
        sprite_cx - box_w / 2.0
    } else if area.anchor_right {
        area.sprite_x as f32 + area.sprite_w as f32 - box_w
    } else {
        area.sprite_x as f32
    }
    .clamp(0.0, (area.canvas_w as f32 - box_w).max(0.0));

    // Vertical: the box edge sits BOX_GAP px from the visible content, the
    // tail spans that gap and stops 1px shy of the sprite.
    let by = if area.above {
        (area.content_top as f32 - gap - box_h).max(0.0)
    } else {
        (area.content_bottom as f32 + gap).min((area.canvas_h as f32 - box_h).max(0.0))
    };

    canvas.rounded_rect(bx, by, box_w, box_h, 9.0 * f, BG);
    let tri_w = TRI_W as f32 * f;
    let tri_x = (sprite_cx - tri_w / 2.0).clamp(bx + 10.0 * f, bx + box_w - tri_w - 10.0 * f);
    if area.above {
        canvas.triangle(tri_x, by + box_h - 1.0, tri_w, gap - f + 1.0, true, BG);
    } else {
        canvas.triangle(tri_x, by - gap + f, tri_w, gap - f, false, BG);
    }

    let tx = (bx + pad) as i32;
    let ty = (by + pad) as i32;
    text.draw(canvas, bubble.label, label_style, wrap_w, tx, ty, LABEL_COLOR, None);
    let visible = bubble.visible_bytes(now_ms).min(body.len());
    text.draw(
        canvas,
        &body,
        body_style,
        wrap_w,
        tx,
        ty + line as i32,
        BODY_COLOR,
        Some(visible),
    );
    (
        bx.round() as i32,
        by.round() as i32,
        box_w.ceil() as u32,
        box_h.ceil() as u32,
    )
}

#[cfg(test)]
mod tests {
    use pet_proto::{Meta, SessionView, Source};

    use super::*;

    fn view(state: AgentState, body: Option<&str>) -> SessionView {
        SessionView {
            key: SessionKey::new(Source::Claude, "s1"),
            state,
            since: 0,
            seen: false,
            via: None,
            body: body.map(String::from),
            meta: Meta::default(),
        }
    }

    fn snap(top: AgentState, sessions: Vec<SessionView>) -> Snapshot {
        Snapshot {
            top,
            sessions,
            unread: 0,
            at: 0,
        }
    }

    #[test]
    fn alerts_only_for_alert_states_with_bodies() {
        let s = snap(
            AgentState::Waiting,
            vec![view(AgentState::Waiting, Some("Approve tool?"))],
        );
        let (key, label, body) = alert_for(&s).unwrap();
        assert_eq!(key.session, "s1");
        assert_eq!(label, "Needs input");
        assert_eq!(body, "Approve tool?");

        // Running is not an alert even with a body.
        assert!(alert_for(&snap(
            AgentState::Running,
            vec![view(AgentState::Running, Some("thinking"))],
        ))
        .is_none());
        // Alert state without a body: no bubble.
        assert!(alert_for(&snap(
            AgentState::Failed,
            vec![view(AgentState::Failed, None)],
        ))
        .is_none());
        // Whitespace body: no bubble.
        assert!(alert_for(&snap(
            AgentState::Failed,
            vec![view(AgentState::Failed, Some("  "))],
        ))
        .is_none());
    }

    #[test]
    fn alert_picks_the_top_priority_session() {
        let s = snap(
            AgentState::Failed,
            vec![
                view(AgentState::Waiting, Some("other")),
                view(AgentState::Failed, Some("crashed")),
            ],
        );
        assert_eq!(alert_for(&s).unwrap().2, "crashed");
    }

    #[test]
    fn typewriter_reveals_at_30ms_per_char() {
        let b = Bubble::new(
            SessionKey::new(Source::Claude, "s"),
            "Needs input",
            "héllo".into(), // 5 chars, 6 bytes
            1000,
        );
        assert_eq!(b.visible_chars(1000), 0);
        assert_eq!(b.visible_chars(1029), 0);
        assert_eq!(b.visible_chars(1030), 1);
        assert_eq!(b.visible_chars(1090), 3);
        assert_eq!(b.visible_chars(9999), 5);
        // Byte cutoffs respect the multibyte é.
        assert_eq!(b.visible_bytes(1030), 1);
        assert_eq!(b.visible_bytes(1060), 3); // h + é(2 bytes)
        assert_eq!(b.visible_bytes(9999), 6);
    }

    #[test]
    fn typing_deadlines_step_then_stop() {
        let b = Bubble::new(
            SessionKey::new(Source::Claude, "s"),
            "Blocked",
            "ab".into(),
            0,
        );
        assert_eq!(b.typing_deadline_ms(0), Some(30));
        assert_eq!(b.typing_deadline_ms(30), Some(60));
        assert_eq!(b.typing_deadline_ms(59), Some(60));
        assert_eq!(b.typing_deadline_ms(60), None);
        // Time never runs backwards past the start.
        let late = Bubble::new(SessionKey::new(Source::Claude, "s"), "x", "ab".into(), 100);
        assert_eq!(late.typing_deadline_ms(0), Some(130));
    }

    #[test]
    fn empty_body_is_instantly_done() {
        let b = Bubble::new(SessionKey::new(Source::Claude, "s"), "Ready", String::new(), 0);
        assert_eq!(b.typing_deadline_ms(0), None);
        assert_eq!(b.visible_bytes(0), 0);
    }

    #[test]
    fn reveal_progress_is_keyed_on_content_identity_only() {
        let body = "Approve the deploy to staging?";
        let alert_snap = |since: i64, noise: bool| {
            let mut sessions = vec![SessionView {
                since,
                ..view(AgentState::Waiting, Some(body))
            }];
            if noise {
                // Unrelated churn: another session, different unread count.
                sessions.push(SessionView {
                    key: SessionKey::new(Source::Codex, "other"),
                    ..view(AgentState::Running, Some("busy"))
                });
            }
            Snapshot {
                unread: if noise { 7 } else { 1 },
                at: since,
                ..snap(AgentState::Waiting, sessions)
            }
        };

        let mut alert = AlertBubble::default();
        assert!(alert.apply(&alert_snap(100, false), 0)); // appears
        assert_eq!(alert.visible().unwrap().visible_chars(90), 3);

        // Heartbeat refresh: same triple, new since/deadline/noise. The
        // reveal keeps its start; nothing visually changes.
        assert!(!alert.apply(&alert_snap(999, true), 90));
        assert_eq!(alert.visible().unwrap().visible_chars(90), 3);

        // Transient clear (e.g. waiting -> running heartbeat around a
        // permission prompt), then the same content returns: progress
        // resumes monotonically, never resets. (Track switches and the
        // 3x-then-idle settle live in Timeline and cannot touch this state;
        // hide/show keeps `AlertBubble` untouched the same way.)
        assert!(alert.apply(&snap(AgentState::Running, vec![]), 120)); // hidden
        assert!(alert.visible().is_none());
        assert!(alert.apply(&alert_snap(2000, true), 150)); // reshown
        assert_eq!(alert.visible().unwrap().visible_chars(150), 5);

        // Fully revealed stays fully revealed for identical content.
        let total = body.chars().count();
        assert_eq!(alert.visible().unwrap().visible_chars(100_000), total);
        assert!(!alert.apply(&alert_snap(3000, false), 100_000));
        assert_eq!(alert.visible().unwrap().visible_chars(100_000), total);

        // A changed body re-types from zero, exactly once.
        let changed = snap(
            AgentState::Waiting,
            vec![view(AgentState::Waiting, Some("Different question"))],
        );
        assert!(alert.apply(&changed, 100_000));
        assert_eq!(alert.visible().unwrap().visible_chars(100_000), 0);
        assert!(!alert.apply(&changed, 100_030));
        assert_eq!(alert.visible().unwrap().visible_chars(100_030), 1);
    }

    #[test]
    fn label_change_with_same_body_re_types() {
        let mut alert = AlertBubble::default();
        let waiting = snap(
            AgentState::Waiting,
            vec![view(AgentState::Waiting, Some("same text"))],
        );
        let failed = snap(
            AgentState::Failed,
            vec![view(AgentState::Failed, Some("same text"))],
        );
        assert!(alert.apply(&waiting, 0));
        assert_eq!(alert.visible().unwrap().visible_chars(300), 9); // done
        assert!(alert.apply(&failed, 300));
        assert_eq!(alert.visible().unwrap().visible_chars(300), 0);
    }
}

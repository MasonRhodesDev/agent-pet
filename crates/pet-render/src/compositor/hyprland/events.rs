//! Pure socket2 line classification. Hyprland emits `EVENT>>DATA` lines; we
//! only care which ones mean "the active toplevel might have changed" and so
//! warrant a fresh socket1 query. The event payloads are intentionally
//! ignored — they are thin/lossy (e.g. `activewindow>>class,title` has no
//! address), so the query is the source of truth.

/// Does this socket2 line imply a possible active-window change?
pub fn is_focus_event(line: &str) -> bool {
    let name = line.split(">>").next().unwrap_or("");
    matches!(
        name,
        // Active toplevel changed (v2 carries the address; both trigger a
        // requery). focusedmon fires on monitor focus moves, which also
        // change the active window. closewindow/openwindow can shift focus.
        "activewindow" | "activewindowv2" | "focusedmon" | "closewindow" | "openwindow"
    )
}

/// Empty `activewindowv2>>` / `activewindow>>,` mean focus left every
/// toplevel (e.g. the last window on a workspace closed). Lets the reader
/// short-circuit to a `None` fact without a query.
pub fn is_focus_cleared(line: &str) -> bool {
    match line.split_once(">>") {
        Some(("activewindowv2", data)) => data.is_empty(),
        Some(("activewindow", data)) => data.is_empty() || data == ",",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_focus_events() {
        assert!(is_focus_event("activewindow>>kitty,claude — repo"));
        assert!(is_focus_event("activewindowv2>>5f3a1c0"));
        assert!(is_focus_event("focusedmon>>DP-1,3"));
        assert!(is_focus_event("closewindow>>5f3a1c0"));
        assert!(is_focus_event("openwindow>>5f3a1c0,1,kitty,title"));
        assert!(!is_focus_event("workspace>>2"));
        assert!(!is_focus_event("createworkspace>>3"));
        assert!(!is_focus_event("monitorremoved>>DP-1"));
        assert!(!is_focus_event(""));
    }

    #[test]
    fn detects_focus_cleared() {
        assert!(is_focus_cleared("activewindowv2>>"));
        assert!(is_focus_cleared("activewindow>>,"));
        assert!(is_focus_cleared("activewindow>>"));
        assert!(!is_focus_cleared("activewindowv2>>5f3a1c0"));
        assert!(!is_focus_cleared("activewindow>>kitty,claude"));
        assert!(!is_focus_cleared("focusedmon>>DP-1,3"));
    }
}

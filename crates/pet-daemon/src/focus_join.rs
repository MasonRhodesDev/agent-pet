//! Join an active toplevel window to a tracked session (focus-aware
//! suppression). Pure over injected `/proc` reads so it is unit-testable.
//!
//! Conservative by design: if two sessions match the same window, the join
//! returns `None`. Wrongly suppressing the *wrong* session hides real
//! information — the cardinal sin — so ambiguity must fail toward "keep
//! nagging," never toward "suppress something."

use std::collections::BTreeMap;

use pet_proto::{ActiveWindow, Meta, SessionKey};

use crate::effects::{ancestry_chain, parse_kitty_pane, read_proc_stat};

/// Resolve which tracked session (if any) owns the active window.
/// `sessions` is (key, meta) for every live session. `read_stat` reads
/// `/proc/<pid>/stat` (injected for tests).
pub fn resolve(
    active: &ActiveWindow,
    sessions: &BTreeMap<SessionKey, Meta>,
    read_stat: impl Fn(u32) -> Option<String> + Copy,
) -> Option<SessionKey> {
    match active.pid {
        Some(win_pid) => resolve_by_pid(win_pid, sessions, read_stat),
        None => resolve_by_title(active, sessions),
    }
}

/// The daemon's live-read entry point.
pub fn resolve_live(
    active: &ActiveWindow,
    sessions: &BTreeMap<SessionKey, Meta>,
) -> Option<SessionKey> {
    resolve(active, sessions, read_proc_stat)
}

fn resolve_by_pid(
    win_pid: u32,
    sessions: &BTreeMap<SessionKey, Meta>,
    read_stat: impl Fn(u32) -> Option<String> + Copy,
) -> Option<SessionKey> {
    // Rung 1 — kitty OS-window pid match (one kitty window per pane here).
    let kitty: Vec<&SessionKey> = sessions
        .iter()
        .filter(|(_, m)| {
            m.pane
                .as_deref()
                .and_then(parse_kitty_pane)
                .is_some_and(|(kpid, _)| kpid == win_pid)
        })
        .map(|(k, _)| k)
        .collect();
    if let Some(one) = unambiguous(&kitty) {
        return Some(one);
    }
    if kitty.len() > 1 {
        return None; // ambiguous kitty match → keep nagging
    }

    // Rung 2 — ancestry: the window pid is the terminal hosting the harness.
    let anc: Vec<&SessionKey> = sessions
        .iter()
        .filter(|(_, m)| {
            m.agent_pid
                .is_some_and(|pid| ancestry_chain(pid, read_stat).contains(&win_pid))
        })
        .map(|(k, _)| k)
        .collect();
    unambiguous(&anc)
}

/// Foreign-toplevel path: no pid, only app_id/title. Only a confident,
/// unambiguous title correlation counts; otherwise `None` (suppression
/// simply doesn't engage — safe).
fn resolve_by_title(
    active: &ActiveWindow,
    sessions: &BTreeMap<SessionKey, Meta>,
) -> Option<SessionKey> {
    let title = active.title.as_deref()?.trim();
    if title.is_empty() {
        return None;
    }
    let matches: Vec<&SessionKey> = sessions
        .iter()
        .filter(|(_, m)| {
            m.title
                .as_deref()
                .is_some_and(|t| !t.is_empty() && title.contains(t))
        })
        .map(|(k, _)| k)
        .collect();
    unambiguous(&matches)
}

fn unambiguous(keys: &[&SessionKey]) -> Option<SessionKey> {
    match keys {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_proto::Source;

    fn sess(pairs: &[(SessionKey, Meta)]) -> BTreeMap<SessionKey, Meta> {
        pairs.iter().cloned().collect()
    }

    fn kitty_meta(pid: u32, wid: u32) -> Meta {
        Meta {
            pane: Some(format!("kitty-{pid}-{wid}")),
            ..Default::default()
        }
    }

    fn win(pid: Option<u32>) -> ActiveWindow {
        ActiveWindow {
            pid,
            ..Default::default()
        }
    }

    #[test]
    fn kitty_pid_match() {
        let k = SessionKey::new(Source::Claude, "a");
        let s = sess(&[(k.clone(), kitty_meta(1234, 1))]);
        assert_eq!(resolve(&win(Some(1234)), &s, |_| None), Some(k));
        assert_eq!(resolve(&win(Some(9999)), &s, |_| None), None);
    }

    #[test]
    fn ancestry_match() {
        // agent pid 500; its stat chain: 500 -> 400 (the terminal window).
        let read = |pid: u32| match pid {
            500 => Some("500 (claude) S 400 1 1".to_string()),
            400 => Some("400 (kitty) S 1 1 1".to_string()),
            _ => None,
        };
        let k = SessionKey::new(Source::Claude, "a");
        let meta = Meta {
            agent_pid: Some(500),
            ..Default::default()
        };
        let s = sess(&[(k.clone(), meta)]);
        assert_eq!(resolve(&win(Some(400)), &s, read), Some(k));
        assert_eq!(resolve(&win(Some(777)), &s, read), None);
    }

    #[test]
    fn ambiguous_kitty_is_none() {
        // Two sessions in the same kitty OS-window pid → conservative None.
        let a = SessionKey::new(Source::Claude, "a");
        let b = SessionKey::new(Source::Codex, "b");
        let s = sess(&[(a, kitty_meta(1234, 1)), (b, kitty_meta(1234, 2))]);
        assert_eq!(resolve(&win(Some(1234)), &s, |_| None), None);
    }

    #[test]
    fn kitty_beats_ancestry() {
        // Session A matches by kitty pid; B would match by ancestry to the
        // same window — kitty (rung 1) wins and returns A unambiguously.
        let read = |pid: u32| (pid == 700).then(|| "700 (claude) S 1234 1 1".to_string());
        let a = SessionKey::new(Source::Claude, "a");
        let b = SessionKey::new(Source::Codex, "b");
        let s = sess(&[
            (a.clone(), kitty_meta(1234, 1)),
            (
                b,
                Meta {
                    agent_pid: Some(700),
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(resolve(&win(Some(1234)), &s, read), Some(a));
    }

    #[test]
    fn foreign_toplevel_title_match() {
        let k = SessionKey::new(Source::Claude, "a");
        let meta = Meta {
            title: Some("fix auth bug".into()),
            ..Default::default()
        };
        let s = sess(&[(k.clone(), meta)]);
        let active = ActiveWindow {
            pid: None,
            title: Some("kitty — fix auth bug".into()),
            ..Default::default()
        };
        assert_eq!(resolve(&active, &s, |_| None), Some(k));
        // No pid and no title correlation → None.
        assert_eq!(resolve(&win(None), &s, |_| None), None);
    }

    #[test]
    fn empty_sessions_is_none() {
        let s = BTreeMap::new();
        assert_eq!(resolve(&win(Some(1)), &s, |_| None), None);
    }
}

//! Effect executors — the only place the daemon touches the outside world.
//!
//! Focus resolution ladder (first rung that lands wins):
//!   a. kitty fast path — `meta.pane` = `kitty-<pid>-<winid>`: raise the OS
//!      window by pid (Hyprland IPC) + `kitten @ focus-window` inside it.
//!   b. tmux — `meta.extra` `tmux_socket`/`tmux_pane` (gt-managed town
//!      agents): switch an attached client to the pane's session, then raise
//!      the OS window hosting that client via its pid ancestry.
//!   c. pid ancestry — walk `meta.agent_pid`'s parent chain and focus the
//!      Hyprland client owning an ancestor pid.
//!   d. miss — one warn describing everything that was tried.
//!
//! Planning (`plan`) is pure and unit-tested; the async executors are thin.

mod hyprland;
mod tmux;

use std::path::Path;

use pet_proto::{Meta, SessionKey, Source};
use tracing::{debug, info, warn};

use crate::config::FocusConfig;

/// One rung of the focus ladder, as pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rung {
    Kitty { kitty_pid: u32, window_id: u32 },
    Tmux { socket: String, pane: String },
    PidAncestry { pid: u32 },
}

/// Outcome of executing one rung.
enum Outcome {
    /// Focus landed, or the rung terminally handled the request (e.g. the
    /// tmux "no client attached" actionable warn).
    Done,
    /// Not applicable or failed — fall through to the next rung.
    Failed(String),
}

/// Decide which rungs apply to a session, in preference order. Pure.
pub fn plan(meta: &Meta) -> Vec<Rung> {
    let mut rungs = Vec::new();
    if let Some((kitty_pid, window_id)) = meta.pane.as_deref().and_then(parse_kitty_pane) {
        rungs.push(Rung::Kitty {
            kitty_pid,
            window_id,
        });
    }
    if let (Some(socket), Some(pane)) = (
        extra_str(meta, "tmux_socket"),
        extra_str(meta, "tmux_pane"),
    ) {
        rungs.push(Rung::Tmux {
            socket: socket.to_owned(),
            pane: pane.to_owned(),
        });
    }
    if let Some(pid) = meta.agent_pid {
        rungs.push(Rung::PidAncestry { pid });
    }
    rungs
}

fn extra_str<'a>(meta: &'a Meta, key: &str) -> Option<&'a str> {
    meta.extra
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Role-aware routing, decided before the generic ladder: Gas Town crew and
/// mayor/escalation sessions get gt-native focus actions. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// gt crew workspace: switch an attached client, or attach one.
    Crew { rig: String, name: String },
    /// The mayor's console; `escalation` additionally drafts the caption
    /// into the mayor's pane.
    Mayor { escalation: bool },
    /// Everything else: the kitty/tmux/pid-ancestry ladder.
    Generic,
}

pub fn route(key: &SessionKey, meta: &Meta) -> Route {
    // Poller-keyed sessions (no direct sibling yet).
    if key.source == Source::Gastown {
        if key.session.starts_with("esc/") {
            return Route::Mayor { escalation: true };
        }
        if key.session == "mayor" {
            return Route::Mayor { escalation: false };
        }
        if let Some((rig, name)) = parse_crew_ref(&key.session) {
            return Route::Crew { rig, name };
        }
        return Route::Generic; // polecats etc.
    }
    // Direct harness sessions tagged by the town intake policy.
    match extra_str(meta, "gastown_ref") {
        Some("mayor") => Route::Mayor { escalation: false },
        Some(gref) => match parse_crew_ref(gref) {
            Some((rig, name)) => Route::Crew { rig, name },
            None => Route::Generic,
        },
        None => Route::Generic,
    }
}

/// `crew/<rig>/<name>` (a `gastown_ref` or a poller session id).
fn parse_crew_ref(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("crew/")?;
    let (rig, name) = rest.split_once('/')?;
    (!rig.is_empty() && !name.is_empty()).then(|| (rig.to_owned(), name.to_owned()))
}

pub async fn focus(
    cfg: &FocusConfig,
    town_dir: &Path,
    key: &SessionKey,
    meta: &Meta,
    body: Option<&str>,
) {
    match route(key, meta) {
        Route::Crew { rig, name } => focus_crew(cfg, town_dir, key, meta, &rig, &name).await,
        Route::Mayor { escalation } => {
            focus_mayor(cfg, town_dir, key, meta, body, escalation).await
        }
        Route::Generic => focus_generic(key, meta).await,
    }
}

async fn focus_generic(key: &SessionKey, meta: &Meta) {
    let rungs = plan(meta);
    if rungs.is_empty() {
        warn!("no focus correlation for {key} (no kitty pane, tmux hint, or agent pid); cannot focus yet");
        return;
    }
    let mut tried: Vec<String> = Vec::new();
    for rung in rungs {
        match execute_rung(&rung).await {
            Outcome::Done => {
                debug!("focused {key} via {rung:?}");
                return;
            }
            Outcome::Failed(why) => tried.push(why),
        }
    }
    warn!("could not focus {key}; tried: {}", tried.join("; "));
}

fn tmux_hints(meta: &Meta) -> Option<(&str, &str)> {
    Some((extra_str(meta, "tmux_socket")?, extra_str(meta, "tmux_pane")?))
}

/// Crew: bring an attached gt client to the workspace; if the session is
/// not loaded anywhere (no client, tmux failure, no correlation at all),
/// attach it in a fresh terminal — Mason: "if they're not loaded, add them
/// to the tmux session".
async fn focus_crew(
    cfg: &FocusConfig,
    town_dir: &Path,
    key: &SessionKey,
    meta: &Meta,
    rig: &str,
    name: &str,
) {
    if let Some((socket, pane)) = tmux_hints(meta) {
        match tmux::switch_to_pane(socket, pane).await {
            tmux::TmuxResult::Switched { client_pid } => {
                if let Err(e) = hyprland::focus_by_ancestry(client_pid).await {
                    debug!("crew client switched but OS window raise skipped: {e}");
                }
                info!("focused crew {rig}/{name} via attached tmux client");
                return;
            }
            tmux::TmuxResult::NoClients { session } => {
                info!("crew session {session:?} has no attached client; spawning attach");
            }
            tmux::TmuxResult::Failed(why) => {
                debug!("crew tmux switch failed ({why}); spawning attach");
            }
        }
    }
    spawn_terminal(cfg, town_dir, &["gt", "crew", "attach", name, "--rig", rig], key);
}

/// Mayor console (also the escalation target): switch an attached client
/// there or spawn one; escalations additionally draft their caption into
/// the mayor's pane — literal keys, NEVER Enter (send-keys targets the
/// pane, so the draft lands even before a spawned client attaches).
async fn focus_mayor(
    cfg: &FocusConfig,
    town_dir: &Path,
    key: &SessionKey,
    meta: &Meta,
    body: Option<&str>,
    escalation: bool,
) {
    let hints = tmux_hints(meta);
    let mut switched = false;
    if let Some((socket, pane)) = hints {
        match tmux::switch_to_pane(socket, pane).await {
            tmux::TmuxResult::Switched { client_pid } => {
                if let Err(e) = hyprland::focus_by_ancestry(client_pid).await {
                    debug!("mayor client switched but OS window raise skipped: {e}");
                }
                info!("focused the mayor via attached tmux client");
                switched = true;
            }
            tmux::TmuxResult::NoClients { session } => {
                info!("mayor session {session:?} has no attached client; spawning attach");
            }
            tmux::TmuxResult::Failed(why) => {
                debug!("mayor tmux switch failed ({why}); spawning attach");
            }
        }
    }
    if !switched {
        spawn_terminal(cfg, town_dir, &["gt", "mayor", "attach"], key);
    }
    if escalation && cfg.escalation_draft {
        match (hints, body) {
            (Some((socket, pane)), Some(body)) => {
                let draft = escalation_draft_line(body);
                match tmux::send_keys_literal(socket, pane, &draft).await {
                    Ok(()) => info!("drafted escalation context into mayor pane {pane}"),
                    Err(e) => warn!("escalation draft send-keys failed: {e}"),
                }
            }
            _ => debug!("no mayor tmux correlation or caption; skipping escalation draft"),
        }
    }
}

/// "Escalation <bead-id>: <title> " from a poller caption
/// ("<bead-id> <title>"). Trailing space; the user finishes or clears it.
fn escalation_draft_line(body: &str) -> String {
    match body.split_once(char::is_whitespace) {
        Some((id, rest)) => format!("Escalation {id}: {} ", rest.trim()),
        None => format!("Escalation {body}: "),
    }
}

/// Detached terminal spawn (own session via setsid, stdio nulled, reaped in
/// the background). Never blocks the effect executor.
fn spawn_terminal(cfg: &FocusConfig, town_dir: &Path, gt_args: &[&str], key: &SessionKey) {
    let mut cmd = tokio::process::Command::new(&cfg.terminal);
    cmd.current_dir(town_dir);
    if cfg.terminal.ends_with("kitty") {
        cmd.arg("--directory").arg(town_dir);
    }
    cmd.args(gt_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            info!(
                "spawned `{} {}` for {key}",
                cfg.terminal,
                gt_args.join(" ")
            );
            tokio::spawn(async move {
                let _ = child.wait().await; // reap
            });
        }
        Err(e) => warn!("failed to spawn terminal {:?} for {key}: {e}", cfg.terminal),
    }
}

async fn execute_rung(rung: &Rung) -> Outcome {
    match rung {
        Rung::Kitty {
            kitty_pid,
            window_id,
        } => focus_kitty(*kitty_pid, *window_id).await,
        Rung::Tmux { socket, pane } => focus_tmux(socket, pane).await,
        Rung::PidAncestry { pid } => match hyprland::focus_by_ancestry(*pid).await {
            Ok(()) => Outcome::Done,
            Err(why) => Outcome::Failed(format!("pid-ancestry({pid}): {why}")),
        },
    }
}

/// Rung a: raise/focus the OS window first (kitty runs one OS window per
/// pane in Mason-style setups; pid matching finds it), then focus the kitty
/// window inside.
async fn focus_kitty(kitty_pid: u32, window_id: u32) -> Outcome {
    hyprland::dispatch_focus_pid(kitty_pid).await;

    let target = format!("unix:@kitty-{kitty_pid}");
    match tokio::process::Command::new("kitten")
        .args(["@", "--to", &target, "focus-window", "--match"])
        .arg(format!("id:{window_id}"))
        .output()
        .await
    {
        Ok(out) if out.status.success() => Outcome::Done,
        Ok(out) => Outcome::Failed(format!(
            "kitty({target}): kitten focus-window failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Outcome::Failed(format!("kitty({target}): kitten not available: {e}")),
    }
}

/// Rung b: switch an already-attached tmux client to the pane, then raise
/// that client's OS window. Never spawns terminals.
async fn focus_tmux(socket: &str, pane: &str) -> Outcome {
    match tmux::switch_to_pane(socket, pane).await {
        tmux::TmuxResult::Switched { client_pid } => {
            // The tmux-side switch already succeeded; OS-window raise is
            // best-effort on top.
            if let Err(e) = hyprland::focus_by_ancestry(client_pid).await {
                debug!("tmux client switched but OS window raise skipped: {e}");
            }
            Outcome::Done
        }
        tmux::TmuxResult::NoClients { session } => {
            // Terminal outcome by design: don't spawn terminals.
            // TODO(config): optionally spawn/attach a terminal here, gated
            // behind a [focus] config knob.
            warn!(
                "tmux session {session:?} has no attached client; attach with: \
                 tmux -S {socket} attach -t {session}"
            );
            Outcome::Done
        }
        tmux::TmuxResult::Failed(why) => Outcome::Failed(format!("tmux({socket},{pane}): {why}")),
    }
}

fn parse_kitty_pane(pane: &str) -> Option<(u32, u32)> {
    let rest = pane.strip_prefix("kitty-")?;
    let (pid, win) = rest.split_once('-')?;
    Some((pid.parse().ok()?, win.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(
        pane: Option<&str>,
        tmux: Option<(&str, &str)>,
        agent_pid: Option<u32>,
    ) -> Meta {
        let mut meta = Meta {
            pane: pane.map(str::to_owned),
            agent_pid,
            ..Default::default()
        };
        if let Some((socket, pane_id)) = tmux {
            meta.extra.insert("tmux_socket".into(), socket.into());
            meta.extra.insert("tmux_pane".into(), pane_id.into());
        }
        meta
    }

    #[test]
    fn plan_kitty_fast_path_first() {
        let meta = meta_with(
            Some("kitty-4242-7"),
            Some(("/tmp/tmux-1000/default", "%3")),
            Some(999),
        );
        assert_eq!(
            plan(&meta),
            vec![
                Rung::Kitty {
                    kitty_pid: 4242,
                    window_id: 7
                },
                Rung::Tmux {
                    socket: "/tmp/tmux-1000/default".into(),
                    pane: "%3".into()
                },
                Rung::PidAncestry { pid: 999 },
            ]
        );
    }

    #[test]
    fn plan_tmux_then_ancestry_for_town_agents() {
        // The live gap: gt town agents — no pane, tmux hints + agent_pid.
        let meta = meta_with(None, Some(("/tmp/gt.sock", "%0")), Some(31337));
        assert_eq!(
            plan(&meta),
            vec![
                Rung::Tmux {
                    socket: "/tmp/gt.sock".into(),
                    pane: "%0".into()
                },
                Rung::PidAncestry { pid: 31337 },
            ]
        );
    }

    #[test]
    fn plan_ancestry_only_when_pid_is_all_we_have() {
        assert_eq!(
            plan(&meta_with(None, None, Some(77))),
            vec![Rung::PidAncestry { pid: 77 }]
        );
    }

    #[test]
    fn plan_skips_malformed_hints() {
        // Unrecognized pane scheme, half a tmux hint, no pid → nothing.
        let mut meta = meta_with(Some("wezterm-1-2"), None, None);
        meta.extra.insert("tmux_socket".into(), "/sock".into());
        assert_eq!(plan(&meta), vec![]);

        // Empty-string tmux values are not hints.
        let meta = meta_with(None, Some(("", "%1")), None);
        assert_eq!(plan(&meta), vec![]);
    }

    #[test]
    fn plan_empty_meta_is_a_miss() {
        assert!(plan(&Meta::default()).is_empty());
    }

    #[test]
    fn routing_prefers_gt_roles_over_the_ladder() {
        let gt = |session: &str| SessionKey::new(Source::Gastown, session);
        let claude = SessionKey::new(Source::Claude, "uuid-1");

        // Poller-keyed sessions.
        assert_eq!(
            route(&gt("esc/hq-5ta"), &Meta::default()),
            Route::Mayor { escalation: true }
        );
        assert_eq!(
            route(&gt("mayor"), &Meta::default()),
            Route::Mayor { escalation: false }
        );
        assert_eq!(
            route(&gt("crew/lifemd/user_merge"), &Meta::default()),
            Route::Crew {
                rig: "lifemd".into(),
                name: "user_merge".into()
            }
        );
        assert_eq!(route(&gt("odin/furiosa"), &Meta::default()), Route::Generic);

        // Direct sessions tagged by intake.
        let mut meta = Meta::default();
        meta.extra
            .insert("gastown_ref".into(), "crew/idp_rc_controller/rc_rollout".into());
        assert_eq!(
            route(&claude, &meta),
            Route::Crew {
                rig: "idp_rc_controller".into(),
                name: "rc_rollout".into()
            }
        );
        let mut meta = Meta::default();
        meta.extra.insert("gastown_ref".into(), "mayor".into());
        assert_eq!(route(&claude, &meta), Route::Mayor { escalation: false });

        // Untagged direct sessions take the ladder.
        assert_eq!(route(&claude, &Meta::default()), Route::Generic);
        // Malformed refs degrade to the ladder, never panic.
        let mut meta = Meta::default();
        meta.extra.insert("gastown_ref".into(), "crew/only-rig".into());
        assert_eq!(route(&claude, &meta), Route::Generic);
    }

    #[test]
    fn escalation_draft_formats_bead_captions() {
        assert_eq!(
            escalation_draft_line("hq-5ta Confirm Block B state"),
            "Escalation hq-5ta: Confirm Block B state "
        );
        assert_eq!(escalation_draft_line("hq-9zz"), "Escalation hq-9zz: ");
    }

    #[test]
    fn kitty_pane_parses() {
        assert_eq!(parse_kitty_pane("kitty-123-4"), Some((123, 4)));
        assert_eq!(parse_kitty_pane("kitty-x-4"), None);
        assert_eq!(parse_kitty_pane("tmux-%1"), None);
    }
}

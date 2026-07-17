//! tmux rung of the focus ladder: bring an already-attached client to the
//! session/pane of a gt-managed town agent. Subprocess-based (`tmux` from
//! PATH), every call capped at 3 s; failures degrade to the next rung.

use std::time::Duration;

use tracing::debug;

const TMUX_TIMEOUT: Duration = Duration::from_secs(3);
const CLIENT_FORMAT: &str = "#{client_tty}:::#{client_pid}:::#{session_name}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub tty: String,
    pub pid: u32,
    pub session: String,
}

pub enum TmuxResult {
    /// A client now shows the target session/pane; raise its OS window next.
    Switched { client_pid: u32 },
    /// The server is up but nobody is attached — the caller warns with the
    /// exact attach command instead of spawning terminals.
    NoClients { session: String },
    /// Anything else (no server, bad socket, tmux missing, timeout).
    Failed(String),
}

/// Resolve the pane's session, pick an attached client, and switch it there.
pub async fn switch_to_pane(socket: &str, pane: &str) -> TmuxResult {
    let session = match run(socket, &["display-message", "-p", "-t", pane, "#{session_name}"])
        .await
    {
        Ok(out) => out.trim().to_owned(),
        Err(e) => return TmuxResult::Failed(format!("resolving session of pane {pane}: {e}")),
    };
    if session.is_empty() {
        return TmuxResult::Failed(format!("pane {pane} resolved to an empty session name"));
    }

    let clients = match run(socket, &["list-clients", "-F", CLIENT_FORMAT]).await {
        Ok(out) => parse_clients(&out),
        Err(e) => return TmuxResult::Failed(format!("listing clients: {e}")),
    };
    let any_attached = !clients.is_empty();
    let clients = eligible_clients(clients, &session);
    let Some(client) = pick_client(&clients, &session) else {
        if any_attached {
            // A client exists but the guard protects it (mayor console).
            return TmuxResult::Failed(format!(
                "refusing to move the attached client off its gt mayor session \
                 to non-crew/mayor session {session:?}"
            ));
        }
        return TmuxResult::NoClients { session };
    };

    if let Err(e) = run(socket, &["switch-client", "-c", &client.tty, "-t", &session]).await {
        return TmuxResult::Failed(format!("switch-client to {session}: {e}"));
    }
    // Inner window/pane selection is best-effort: the client already sits on
    // the right session even if these fail.
    for args in [["select-window", "-t", pane], ["select-pane", "-t", pane]] {
        if let Err(e) = run(socket, &args).await {
            debug!("tmux {} {pane} failed (session switch stands): {e}", args[0]);
        }
    }
    TmuxResult::Switched {
        client_pid: client.pid,
    }
}

/// Parse `list-clients -F '#{client_tty}:::#{client_pid}:::#{session_name}'`
/// output. Malformed lines are dropped. Pure.
pub fn parse_clients(output: &str) -> Vec<Client> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ":::");
            let tty = fields.next()?.trim();
            let pid: u32 = fields.next()?.trim().parse().ok()?;
            let session = fields.next()?.trim();
            (!tty.is_empty() && !session.is_empty()).then(|| Client {
                tty: tty.to_owned(),
                pid,
                session: session.to_owned(),
            })
        })
        .collect()
}

/// Prefer a client already viewing the target session (no visual jump for
/// anyone else); otherwise commandeer the first attached client. Pure.
pub fn pick_client<'a>(clients: &'a [Client], target_session: &str) -> Option<&'a Client> {
    clients
        .iter()
        .find(|c| c.session == target_session)
        .or_else(|| clients.first())
}

/// Gas Town session-name role, from the gt naming scheme
/// `<beads-prefix>-<role>` (live: `hq-mayor`, `lmd-crew-user_merge`,
/// `od-witness`, `irc-refinery`, ...). Pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GtSessionKind {
    Mayor,
    Crew,
    Other,
}

pub fn gt_session_kind(name: &str) -> GtSessionKind {
    if name == "mayor" || name.ends_with("-mayor") {
        GtSessionKind::Mayor
    } else if name.contains("-crew-") {
        GtSessionKind::Crew
    } else {
        GtSessionKind::Other
    }
}

/// Guard: never yank a human off the mayor's console for anything that is
/// not crew/mayor (belt-and-braces against stale sessions that predate the
/// town-infra intake filter). Pure.
pub fn eligible_clients(clients: Vec<Client>, target_session: &str) -> Vec<Client> {
    let target_kind = gt_session_kind(target_session);
    clients
        .into_iter()
        .filter(|c| {
            gt_session_kind(&c.session) != GtSessionKind::Mayor
                || target_kind != GtSessionKind::Other
        })
        .collect()
}

/// Type literal text into a pane as a draft — `-l` (no key-name lookup) and
/// deliberately NO Enter: the human submits or clears it.
pub async fn send_keys_literal(socket: &str, pane: &str, text: &str) -> anyhow::Result<()> {
    run(socket, &["send-keys", "-l", "-t", pane, text])
        .await
        .map(|_| ())
}

async fn run(socket: &str, args: &[&str]) -> anyhow::Result<String> {
    let fut = tokio::process::Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output();
    let out = tokio::time::timeout(TMUX_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow::anyhow!("tmux {} timed out", args.first().unwrap_or(&"?")))??;
    anyhow::ensure!(
        out.status.success(),
        "tmux {} failed: {}",
        args.first().unwrap_or(&"?"),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(tty: &str, pid: u32, session: &str) -> Client {
        Client {
            tty: tty.into(),
            pid,
            session: session.into(),
        }
    }

    #[test]
    fn parses_client_lines_and_drops_garbage() {
        let out = "/dev/pts/3:::1200:::town\n\
                   /dev/pts/9:::1300:::scratch\n\
                   not-a-client-line\n\
                   /dev/pts/5:::NaN:::town\n\
                   :::1400:::town\n";
        assert_eq!(
            parse_clients(out),
            vec![
                client("/dev/pts/3", 1200, "town"),
                client("/dev/pts/9", 1300, "scratch"),
            ]
        );
        assert!(parse_clients("").is_empty());
    }

    #[test]
    fn prefers_client_already_on_target_session() {
        let clients = vec![
            client("/dev/pts/1", 100, "other"),
            client("/dev/pts/2", 200, "town"),
        ];
        assert_eq!(pick_client(&clients, "town").unwrap().pid, 200);
    }

    #[test]
    fn mayor_console_is_never_stolen_for_infra() {
        // Regression: clicking a town-infra bubble retargeted Mason's
        // hq-mayor client to od-witness.
        let mayor_client = vec![client("/dev/pts/21", 100, "hq-mayor")];

        assert!(eligible_clients(mayor_client.clone(), "od-witness").is_empty());
        assert!(eligible_clients(mayor_client.clone(), "default").is_empty());
        // Crew and mayor targets stay allowed.
        assert_eq!(
            eligible_clients(mayor_client.clone(), "lmd-crew-user_merge").len(),
            1
        );
        assert_eq!(eligible_clients(mayor_client, "hq-mayor").len(), 1);

        // Non-mayor clients can be moved anywhere.
        let other = vec![client("/dev/pts/9", 200, "scratch")];
        assert_eq!(eligible_clients(other, "od-witness").len(), 1);
    }

    #[test]
    fn gt_session_names_classify() {
        assert_eq!(gt_session_kind("hq-mayor"), GtSessionKind::Mayor);
        assert_eq!(gt_session_kind("mayor"), GtSessionKind::Mayor);
        assert_eq!(gt_session_kind("lmd-crew-user_merge"), GtSessionKind::Crew);
        assert_eq!(gt_session_kind("irc-crew-rc_rollout"), GtSessionKind::Crew);
        assert_eq!(gt_session_kind("od-witness"), GtSessionKind::Other);
        assert_eq!(gt_session_kind("hq-deacon"), GtSessionKind::Other);
        assert_eq!(gt_session_kind("pettest"), GtSessionKind::Other);
    }

    #[test]
    fn falls_back_to_first_attached_client() {
        let clients = vec![
            client("/dev/pts/1", 100, "other"),
            client("/dev/pts/2", 200, "scratch"),
        ];
        assert_eq!(pick_client(&clients, "town").unwrap().pid, 100);
        assert!(pick_client(&[], "town").is_none());
    }
}

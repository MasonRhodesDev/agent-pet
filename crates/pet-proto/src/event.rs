use serde::{Deserialize, Serialize};

use crate::key::SessionKey;

/// Highest wire schema version this build understands. Within a version the
/// schema evolves additively only: new fields are `Option`/defaulted, unknown
/// fields are tolerated.
pub const PROTOCOL_VERSION: u32 = 1;

/// One observation about one session, produced by a harness adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(default = "default_version")]
    pub v: u32,
    pub source: Source,
    /// Harness-native session id (claude session_id, codex thread id,
    /// `gastown:<rig>/<polecat>`, happy CUID, ...).
    pub session: String,
    pub state: AgentState,
    /// Human caption for the tray row (prompt excerpt, bead title, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Epoch milliseconds. The daemon stamps receive time when absent and
    /// clamps future timestamps to its own clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<i64>,
    /// Wrapper provenance when this signal was observed by a harness that
    /// wraps another (Happy, Gas Town). Absent = direct observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    #[serde(default)]
    pub meta: Meta,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Running,
    /// Needs input: approval prompt or question pending.
    Waiting,
    /// Turn finished with unseen output. Maps to the `review` pet.json track.
    Ready,
    /// Blocked: error, stale lease, crashed worker.
    Failed,
    /// Session alive with nothing pending (explicit clear).
    #[default]
    Idle,
    /// Session ended; remove it.
    Gone,
}

impl AgentState {
    /// Human label, matching the ChatGPT pet's vocabulary.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Waiting => "Needs input",
            Self::Ready => "Ready",
            Self::Failed => "Blocked",
            Self::Idle => "Idle",
            Self::Gone => "Gone",
        }
    }

    /// Name of the pet.json animation track for this state.
    pub fn track(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Ready => "review",
            Self::Failed => "failed",
            Self::Idle | Self::Gone => "idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Claude,
    Codex,
    Gastown,
    Happy,
    Openclaw,
    Pi,
    Other,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gastown => "gastown",
            Self::Happy => "happy",
            Self::Openclaw => "openclaw",
            Self::Pi => "pi",
            Self::Other => "other",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for Source {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "gastown" => Self::Gastown,
            "happy" => Self::Happy,
            "openclaw" => Self::Openclaw,
            "pi" => Self::Pi,
            "other" => Self::Other,
            _ => return Err(ValidationError::UnknownSource),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Via {
    pub wrapper: Source,
    pub wrapper_session: String,
    /// Canonical child key when the wrapper knows exactly which direct
    /// session it is relaying (seeds the daemon's alias table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<SessionKey>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Terminal pane correlation for click-to-focus (optional enrichment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Pid of the harness process (emitter's getppid). Join hint for wrapper
    /// dedup: Happy reports the same pid as `hostPid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Meta {
    /// Overlay `other`'s populated fields onto `self`, keeping existing
    /// values. Used when wrapper events are only allowed to enrich.
    pub fn fill_from(&mut self, other: &Meta) {
        fn fill(dst: &mut Option<String>, src: &Option<String>) {
            if dst.is_none() {
                dst.clone_from(src);
            }
        }
        fill(&mut self.cwd, &other.cwd);
        fill(&mut self.tty, &other.tty);
        fill(&mut self.model, &other.model);
        fill(&mut self.title, &other.title);
        fill(&mut self.pane, &other.pane);
        fill(&mut self.transcript_path, &other.transcript_path);
        fill(&mut self.tool, &other.tool);
        if self.agent_pid.is_none() {
            self.agent_pid = other.agent_pid;
        }
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    #[error("session id is empty")]
    EmptySession,
    #[error("via.wrapper must differ from source")]
    SelfVia,
    #[error("unknown source")]
    UnknownSource,
    #[error("malformed session key (expected source/session)")]
    BadSessionKey,
}

impl Event {
    /// Validate and normalize an incoming event. `now_ms` is the daemon's
    /// clock; missing timestamps are stamped with it and future timestamps
    /// are clamped to it.
    pub fn validate(mut self, now_ms: i64) -> Result<Self, ValidationError> {
        if self.v > PROTOCOL_VERSION {
            return Err(ValidationError::UnsupportedVersion);
        }
        if self.session.is_empty() {
            return Err(ValidationError::EmptySession);
        }
        if let Some(via) = &self.via {
            if via.wrapper == self.source {
                return Err(ValidationError::SelfVia);
            }
        }
        match self.ts {
            Some(ts) if ts <= now_ms => {}
            _ => self.ts = Some(now_ms),
        }
        Ok(self)
    }

    pub fn key(&self) -> SessionKey {
        SessionKey::new(self.source, self.session.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_full_event() {
        let ev = Event {
            v: 1,
            source: Source::Claude,
            session: "abc-123".into(),
            state: AgentState::Waiting,
            body: Some("Permission needed".into()),
            ts: Some(1000),
            via: Some(Via {
                wrapper: Source::Happy,
                wrapper_session: "cuid".into(),
                links: Some(SessionKey::new(Source::Claude, "abc-123")),
            }),
            meta: Meta {
                cwd: Some("/home/mason".into()),
                agent_pid: Some(4242),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session, ev.session);
        assert_eq!(back.state, AgentState::Waiting);
        assert_eq!(back.via, ev.via);
        assert_eq!(back.meta, ev.meta);
    }

    #[test]
    fn tolerates_unknown_fields_and_missing_optionals() {
        let json = r#"{
            "source": "codex", "session": "t1", "state": "running",
            "some_future_field": {"x": 1},
            "meta": {"cwd": "/tmp", "novel_hint": "yes"}
        }"#;
        let ev: Event = serde_json::from_str(json).unwrap();
        assert_eq!(ev.v, 1);
        assert_eq!(ev.source, Source::Codex);
        assert!(ev.via.is_none());
        assert_eq!(ev.meta.extra.get("novel_hint").unwrap(), "yes");
    }

    #[test]
    fn validate_stamps_and_clamps_ts() {
        let mk = |ts| Event {
            v: 1,
            source: Source::Claude,
            session: "s".into(),
            state: AgentState::Running,
            body: None,
            ts,
            via: None,
            meta: Meta::default(),
        };
        assert_eq!(mk(None).validate(500).unwrap().ts, Some(500));
        assert_eq!(mk(Some(9999)).validate(500).unwrap().ts, Some(500));
        assert_eq!(mk(Some(400)).validate(500).unwrap().ts, Some(400));
    }

    #[test]
    fn validate_rejects_bad_events() {
        let base = Event {
            v: 1,
            source: Source::Claude,
            session: "s".into(),
            state: AgentState::Running,
            body: None,
            ts: None,
            via: None,
            meta: Meta::default(),
        };
        let mut e = base.clone();
        e.v = PROTOCOL_VERSION + 1;
        assert_eq!(e.validate(0), Err(ValidationError::UnsupportedVersion));

        let mut e = base.clone();
        e.session = String::new();
        assert_eq!(e.validate(0), Err(ValidationError::EmptySession));

        let mut e = base;
        e.via = Some(Via {
            wrapper: Source::Claude,
            wrapper_session: "w".into(),
            links: None,
        });
        assert_eq!(e.validate(0), Err(ValidationError::SelfVia));
    }

    #[test]
    fn session_key_parses_and_displays() {
        let key: SessionKey = "gastown/idp/chrome".parse().unwrap();
        assert_eq!(key.source, Source::Gastown);
        assert_eq!(key.session, "idp/chrome");
        assert_eq!(key.to_string(), "gastown/idp/chrome");
        assert!("nope".parse::<SessionKey>().is_err());
        assert!("gastown/".parse::<SessionKey>().is_err());
    }
}

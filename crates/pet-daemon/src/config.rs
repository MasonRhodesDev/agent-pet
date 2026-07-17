use std::path::PathBuf;

use pet_core::Ttls;
use serde::Deserialize;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct Config {
    pub ttls: Ttls,
    pub gastown: GastownConfig,
    pub focus: FocusConfig,
    pub pet: PetConfig,
    pub state_path: PathBuf,
}

/// Appearance. `skin` names a directory under
/// `~/.config/agent-pet/pets/<skin>/` (containing pet.json + spritesheet), or
/// an absolute path to one. Empty = the installed default pet.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PetConfig {
    pub skin: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FocusConfig {
    /// Terminal spawned to attach gt sessions that have no attached client.
    pub terminal: String,
    /// Draft (never submit) escalation context into the mayor's pane on an
    /// escalation click.
    pub escalation_draft: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            terminal: "kitty".into(),
            escalation_draft: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GastownConfig {
    /// None = auto (enabled when town_dir exists).
    pub enabled: Option<bool>,
    pub poll_secs: u64,
    pub town_dir: PathBuf,
    /// Ephemeral polecats are noise for the pet by default: the human-facing
    /// signals are the mayor, crew, and escalations.
    pub include_polecats: bool,
}

impl Default for GastownConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            poll_secs: 15,
            town_dir: home().join("agent-town/town"),
            include_polecats: false,
        }
    }
}

impl GastownConfig {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or_else(|| self.town_dir.is_dir())
    }
}

/// On-disk shape (all optional). Expiry values are humane strings ("3m",
/// "24h") or raw milliseconds.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    expiry: ExpiryConfig,
    adapters: AdaptersConfig,
    focus: FocusConfig,
    pet: PetConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AdaptersConfig {
    gastown: GastownConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExpiryConfig {
    running: Option<String>,
    waiting: Option<String>,
    ready: Option<String>,
    failed: Option<String>,
    idle_gc: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        let path = config_home().join("agent-pet/config.toml");
        let file: FileConfig = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                warn!("ignoring malformed {}: {e}", path.display());
                FileConfig::default()
            }),
            Err(_) => FileConfig::default(),
        };

        let mut ttls = Ttls::default();
        let overrides = [
            (&file.expiry.running, &mut ttls.running_ms),
            (&file.expiry.waiting, &mut ttls.waiting_ms),
            (&file.expiry.ready, &mut ttls.ready_ms),
            (&file.expiry.failed, &mut ttls.failed_ms),
            (&file.expiry.idle_gc, &mut ttls.idle_gc_ms),
        ];
        for (value, slot) in overrides {
            if let Some(text) = value {
                match parse_duration_ms(text) {
                    Some(ms) => *slot = ms,
                    None => warn!("ignoring bad expiry duration {text:?}"),
                }
            }
        }

        Self {
            ttls,
            gastown: file.adapters.gastown,
            focus: file.focus,
            pet: file.pet,
            state_path: state_home().join("agent-pet/state.json"),
        }
    }
}

fn parse_duration_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(ms) = text.parse::<i64>() {
        return Some(ms);
    }
    let (num, unit) = text.split_at(text.len().checked_sub(1)?);
    let num: i64 = num.trim().parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(num * mult)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"))
}

fn state_home() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/state"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_ms("3m"), Some(180_000));
        assert_eq!(parse_duration_ms("24h"), Some(86_400_000));
        assert_eq!(parse_duration_ms("7d"), Some(604_800_000));
        assert_eq!(parse_duration_ms("1500"), Some(1500));
        assert_eq!(parse_duration_ms("nope"), None);
    }
}

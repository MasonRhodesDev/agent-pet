//! Exact config snippets to merge into harness configs. agent-pet never
//! edits harness config files itself: the codex trust gate and any dotfile
//! manager (chezmoi) must stay in charge.

use std::process::ExitCode;

const CLAUDE: &str = r#"# Merge into ~/.claude/settings.json under "hooks".
# Each event key holds an ARRAY of matcher-groups: APPEND these groups,
# leave your existing groups untouched. "Notification" is usually a new key.
{
  "SessionStart":     [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }],
  "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }],
  "PostToolUse":      [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }],
  "Notification":     [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }],
  "Stop":             [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }],
  "SessionEnd":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "agent-pet emit claude", "timeout": 5 }] }]
}
"#;

const CODEX: &str = r#"# Codex needs BOTH taps:

# 1) ~/.codex/config.toml — add at the VERY TOP of the file, before the
#    first [table] header (top-level TOML keys cannot follow a table).
#    Note: any `notify = false` you may see under [notice] is an unrelated
#    setting; do not touch it.

notify = ["agent-pet", "emit", "codex-notify"]

# 2) ~/.codex/hooks.json — append this entry to the hooks array of each of
#    these events: SessionStart, UserPromptSubmit, PostToolUse (matcher
#    ".*"), PermissionRequest, Stop:

{ "type": "command", "command": "agent-pet emit codex", "timeout": 5 }

#    permission_request gives the pet the actual command/file being approved;
#    stop carries the last assistant message as the "ready" caption.

# 3) Trust gate: codex refuses unapproved hooks. After editing hooks.json (any
#    time you add or change an event — including adding permission_request),
#    run `codex` once interactively and approve the trust prompt; codex then
#    rewrites the [hooks.state] hashes in config.toml itself. Verify with:
#    agent-pet doctor
"#;

pub fn run(harness: &str) -> ExitCode {
    match harness {
        "claude" => {
            print!("{CLAUDE}");
            ExitCode::SUCCESS
        }
        "codex" => {
            print!("{CODEX}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("agent-pet: no config snippet for '{other}' (try: claude, codex)");
            ExitCode::from(2)
        }
    }
}

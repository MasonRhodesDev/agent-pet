# agent-pet

A desktop pet for your AI agents. A floating Wayland mascot whose animation
reflects what your coding agents are doing across harnesses — Claude Code,
Codex CLI, Gas Town today; Happy/OpenClaw and pi next — with an activity
tray for jumping to the session that needs you.

Think the ChatGPT desktop app's pet, but standalone, harness-agnostic,
compositor-agnostic (wlr-layer-shell: Hyprland, Sway, Niri, KWin, COSMIC),
and skinnable with any community Codex pet (`pet.json` + spritesheet).

## How it works

```
harness hooks ──► agent-pet emit <harness> ──► D-Bus ──► agent-petd
  (claude, codex, …)     fire-and-forget            │  pure FSM per session:
                                                    │  running / needs-input /
gas town ◄─── in-daemon poller (gt/bd CLIs) ────────┤  ready / blocked + expiry
                                                    ▼
                              layer-shell mascot + activity tray (SCTK)
```

- Events are `{source, session, state, body, via, meta}`; adapters normalize,
  the daemon's state machine aggregates (priority: needs-input > blocked >
  ready > running), unread results persist until you've seen them.
- Wrapper harnesses (Happy, Gas Town) are deduplicated against direct feeds:
  direct events win, wrappers enrich, a wrapper's `gone` is always honored.
- The emitter never blocks or fails a harness hook: 500 ms hard cap, exit 0.
  D-Bus activation revives a crashed daemon on the next event.

## Install (development)

```sh
cargo build --release
ln -s $PWD/target/release/agent-pet ~/.local/bin/
ln -s $PWD/target/release/agent-petd ~/.local/bin/
# unit + dbus activation (adjust ExecStart paths or use the packaged RPM)
cp dist/agent-petd.service ~/.config/systemd/user/
cp dist/io.github.masonrhodesdev.AgentPet.service ~/.local/share/dbus-1/services/
systemctl --user daemon-reload && systemctl --user enable --now agent-petd
```

Wire up your harnesses — agent-pet never edits their configs itself:

```sh
agent-pet print-config claude   # snippet for ~/.claude/settings.json
agent-pet print-config codex    # snippets + trust-gate walkthrough
agent-pet doctor                # verify everything end to end
agent-pet status                # the aggregated session list, in your terminal
```

## The mascot

- Animates per the highest-priority session state (needs-input, blocked,
  ready, running, idle); state animations play three passes, then settle.
- **Speech bubble**: when a session needs you (needs input / blocked /
  unseen ready) and has a caption, the pet says it — typewriter reveal,
  click-through, capped at 3 lines.
- **Drag to move** (left-drag; ~4 px click threshold). Position persists in
  `$XDG_STATE_HOME/agent-pet/position.json`.
- **Right-click hides** the pet (persists across restarts); bring it back
  with `agent-pet show`, banish it with `agent-pet hide`.

## Skins

Drop any Codex-format pet into `~/.config/agent-pet/pets/<id>/`
(`pet.json` + `spritesheet.{png,webp}`). State tracks used: `running`,
`waiting`, `review`, `failed`, `idle`.

## Workspace layout

| crate | role |
|---|---|
| `pet-proto` | wire event contract, snapshot, session keys (leaf) |
| `pet-core` | pure aggregation FSM — no I/O, fully table-tested |
| `pet-adapters` | harness payload → event mapping (pure) |
| `pet-cli` | `agent-pet`: hook emitters + control CLI |
| `pet-daemon` | `agent-petd`: D-Bus service, runtime, pollers, effects |
| `pet-render` | SCTK layer-shell mascot + tray |

MIT.

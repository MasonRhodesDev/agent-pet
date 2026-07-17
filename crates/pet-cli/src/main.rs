//! `agent-pet` — the thin CLI.
//!
//! `emit *` subcommands are invoked by harness hooks: they must be fast and
//! must NEVER fail the hook, so every error path prints to stderr and exits
//! 0. Control subcommands (`status`, `seen`, `focus`, `doctor`) talk to the
//! running daemon over D-Bus.

mod bus;
mod doctor;
mod emit;
mod print_config;

use std::process::ExitCode;

const USAGE: &str = "\
agent-pet <command>

  emit claude              map a Claude Code hook payload (stdin) and send it
  emit codex               map a Codex hooks.json payload (stdin) and send it
  emit codex-notify <json> map a Codex notify payload (argv) and send it
  status                   print the daemon's aggregated snapshot
  seen [<key>|--all]       mark session(s) seen
  focus <key>              focus the window behind a session
  show | hide              map / unmap the mascot (persists)
  print-config <harness>   print the exact config to merge (claude|codex)
  doctor                   check the installation end to end
";

fn main() -> ExitCode {
    // Behave like a normal Unix CLI when piped to head & co: die on EPIPE
    // instead of panicking on println.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    match argv.as_slice() {
        ["emit", rest @ ..] => emit::run(rest), // always ExitCode::SUCCESS
        ["status"] => run_async(bus::status()),
        ["seen", "--all"] => run_async(bus::seen_all()),
        ["seen", key] => run_async(bus::seen(key)),
        ["focus", key] => run_async(bus::focus(key)),
        ["show"] => run_async(bus::set_visible(true)),
        ["hide"] => run_async(bus::set_visible(false)),
        ["print-config", harness] => print_config::run(harness),
        ["doctor"] => run_async(doctor::run()),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_async(fut: impl std::future::Future<Output = anyhow::Result<()>>) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("agent-pet: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(fut) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("agent-pet: {e:#}");
            ExitCode::FAILURE
        }
    }
}

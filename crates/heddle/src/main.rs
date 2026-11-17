//! heddle: Fabric's command-line tool.
//!
//! `heddle hook claude-code <Event>` is the hook line for every Claude Code
//! event: the PHI brake first, then coordination. The agent verbs (`next`,
//! `claim`, `release`, `done`, `pause`, `status`) and the person's (`stop`,
//! `resume`) work the local room. Arguments are parsed by hand; there is no
//! parser crate.

use std::process::ExitCode;

/// `println!` that never panics: output cut short (`heddle status | head`)
/// is not a fault.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

mod coord;
mod edge;
mod hookrun;
mod install;
mod room;
mod state;
mod wiring;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("hook") => hookrun::run(args.get(2..).unwrap_or_default()),
        Some("setup") => install::setup(args.get(2..).unwrap_or_default()),
        Some("doctor") => install::doctor(args.get(2..).unwrap_or_default()),
        Some("repair") => install::repair(),
        Some("uninstall") => install::uninstall(),
        Some(
            verb @ ("next" | "claim" | "release" | "done" | "pause" | "stop" | "resume" | "status"),
        ) => coord::verb(verb, args.get(2..).unwrap_or_default()),
        Some("phi") => install::phi(args.get(2..).unwrap_or_default()),
        Some("version") => {
            out!("heddle {VERSION}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("heddle: unknown command");
            ExitCode::from(1)
        }
    }
}

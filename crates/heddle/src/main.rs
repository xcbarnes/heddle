//! heddle: Fabric's command-line tool.
//!
//! v0.1 starts with the PHI brake: `heddle hook claude-code <Event>` is the
//! hook line for every Claude Code event. Coordination (the room, claims,
//! news) comes next. Arguments are parsed by hand; there is no parser crate.

use std::process::ExitCode;

mod hookrun;
mod state;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("hook") => hookrun::run(args.get(2..).unwrap_or_default()),
        Some("version") => {
            println!("heddle {VERSION}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("heddle: unknown command");
            ExitCode::from(1)
        }
    }
}

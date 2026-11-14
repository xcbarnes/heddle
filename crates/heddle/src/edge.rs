//! Every program heddle starts, in one place. clippy refuses
//! `std::process::Command` everywhere else, and [`Program`] is the whole
//! list: a new program means a new variant, which the test below names.
//!
//! No hook runs any of these. `setup`, `doctor` and `uninstall` do, when a
//! person (or an agent on their behalf) runs them.
#![allow(clippy::disallowed_types)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A program heddle may start, always by absolute path.
#[derive(Debug)]
pub enum Program {
    /// `/usr/bin/git`, with the repo's fsmonitor and hooks switched off so a
    /// hostile `.git/config` runs nothing.
    Git,
    /// A heddle binary, run once with `version` to prove it starts.
    Heddle(PathBuf),
}

const GIT: &str = "/usr/bin/git";

impl Program {
    fn path(&self) -> &Path {
        match self {
            Program::Git => Path::new(GIT),
            Program::Heddle(p) => p,
        }
    }
}

/// Run a program in `dir` and return its standard output, or `None` if it
/// could not start or exited non-zero. Standard input is empty.
pub fn run(program: &Program, dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new(program.path());
    if matches!(program, Program::Git) {
        cmd.args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ]);
    }
    let out = cmd
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_programs_heddle_starts_are_git_and_itself_by_absolute_path() {
        assert_eq!(Program::Git.path(), Path::new("/usr/bin/git"));
        assert!(Program::Git.path().is_absolute());
        let me = Program::Heddle(PathBuf::from("/x/heddle"));
        assert_eq!(me.path(), Path::new("/x/heddle"));
    }
}

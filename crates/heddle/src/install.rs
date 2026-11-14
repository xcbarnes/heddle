//! The commands a person runs, or asks an agent to run: `setup`, `doctor`,
//! `repair`, `uninstall` and `status`. None of them runs from a hook. They
//! find heddle's folders through `HOME`; the hook lines carry absolute paths
//! and never read the environment.
//!
//! Layout, all under the home folder:
//! - `.local/bin/heddle`: the binary the hook lines run;
//! - `Library/Application Support/fabric/`: `machine/` (the state folder),
//!   `kept/heddle` (the copy `repair` puts back), `receipt`, `REPAIR.txt`.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use fabric_core::detect;
use fabric_core::hook::rule_name;
use fabric_core::json;

use crate::edge::{self, Program};
use crate::state::Machine;
use crate::wiring::Wiring;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit CI built from; a local build says so.
const COMMIT: &str = match option_env!("HEDDLE_COMMIT") {
    Some(c) => c,
    None => "local build",
};
const SETTINGS: &str = ".claude/settings.local.json";
const EXCLUDE_LINE: &str = "/.claude/settings.local.json";
/// Bounds on the dry scan's walk of user and plugin command folders.
const WALK_DEPTH: usize = 12;
const WALK_FILES: usize = 50_000;
const SCAN_CAP: u64 = 16 * 1024 * 1024;

/// heddle's places under the home folder.
struct Paths {
    home: PathBuf,
    bin: PathBuf,
    fabric: PathBuf,
    state: PathBuf,
    kept: PathBuf,
    receipt: PathBuf,
    repair: PathBuf,
}

impl Paths {
    fn new() -> Result<Paths, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute());
        let home = home.ok_or("HOME is not set to an absolute folder")?;
        let fabric = home.join("Library/Application Support/fabric");
        Ok(Paths {
            bin: home.join(".local/bin/heddle"),
            state: fabric.join("machine"),
            kept: fabric.join("kept/heddle"),
            receipt: fabric.join("receipt"),
            repair: fabric.join("REPAIR.txt"),
            fabric,
            home,
        })
    }
}

/// A repository's main checkout and git common folder.
struct Repo {
    main: PathBuf,
    common: PathBuf,
}

/// The repository holding `start`, found from git's own files without
/// running git. A linked worktree leads to its main checkout. A bare
/// repository, or a git folder kept elsewhere, is not supported.
fn find_repo(start: &Path) -> Option<Repo> {
    for dir in start.ancestors() {
        let dot = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot) else {
            continue;
        };
        let common = if meta.is_dir() {
            fs::canonicalize(&dot).ok()?
        } else {
            let text = fs::read_to_string(&dot).ok()?;
            let gitdir = dir.join(text.trim().strip_prefix("gitdir:")?.trim());
            let up = fs::read_to_string(gitdir.join("commondir")).ok()?;
            fs::canonicalize(gitdir.join(up.trim())).ok()?
        };
        if common.file_name()? != ".git" {
            return None;
        }
        return Some(Repo {
            main: common.parent()?.to_path_buf(),
            common,
        });
    }
    None
}

// ---- small file helpers ----

fn utf8(p: &Path) -> Result<String, String> {
    p.to_str()
        .map(String::from)
        .ok_or_else(|| format!("{} is not a UTF-8 path", p.display()))
}

fn tmp_beside(p: &Path) -> PathBuf {
    let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".heddle-tmp-{}", std::process::id()));
    p.with_file_name(name)
}

/// Write through a temporary file and a rename, so a reader sees the old
/// file or the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let tmp = tmp_beside(path);
    let done = fs::write(&tmp, bytes)
        .and_then(|()| fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)))
        .and_then(|()| fs::rename(&tmp, path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done.map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn copy_atomic(from: &Path, to: &Path) -> Result<(), String> {
    let bytes = fs::read(from).map_err(|e| format!("could not read {}: {e}", from.display()))?;
    write_atomic(to, &bytes, 0o755)
}

fn mkdirs(dir: &Path, mode: u32) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A file's size and modification time, as the receipt records them.
fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(p).ok()?;
    Some((m.len(), secs(m.modified().ok()?)))
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("heddle: {msg}");
    ExitCode::from(1)
}

// ---- the receipt ----

/// What setup did, as `key value` lines: kept for `doctor`, `repair` and
/// `uninstall`, and nothing else. Repos are `repo brake|count <path>`.
#[derive(Debug, Default)]
struct Receipt {
    lines: Vec<(String, String)>,
}

impl Receipt {
    fn read(path: &Path) -> Receipt {
        let text = fs::read_to_string(path).unwrap_or_default();
        let lines = text
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(' '))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Receipt { lines }
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn set(&mut self, key: &str, value: String) {
        self.lines.retain(|(k, _)| k != key);
        self.lines.push((key.to_string(), value));
    }
    /// The repos setup wired: (count mode, main checkout).
    fn repos(&self) -> Vec<(bool, PathBuf)> {
        self.lines
            .iter()
            .filter(|(k, _)| k == "repo")
            .filter_map(|(_, v)| v.split_once(' '))
            .map(|(mode, path)| (mode == "count", PathBuf::from(path)))
            .collect()
    }
    fn set_repo(&mut self, main: &Path, count_only: bool) {
        self.lines.retain(|(k, v)| {
            k != "repo" || v.split_once(' ').is_none_or(|(_, p)| Path::new(p) != main)
        });
        let mode = if count_only { "count" } else { "brake" };
        self.lines
            .push(("repo".to_string(), format!("{mode} {}", main.display())));
    }
    fn matches(&self, prefix: &str, p: &Path) -> bool {
        let want = (
            self.get(&format!("{prefix}-size")),
            self.get(&format!("{prefix}-mtime")),
        );
        stamp(p).is_some_and(|(size, mtime)| {
            want == (
                Some(size.to_string().as_str()),
                Some(mtime.to_string().as_str()),
            )
        })
    }
    fn record(&mut self, prefix: &str, p: &Path) {
        if let Some((size, mtime)) = stamp(p) {
            self.set(&format!("{prefix}-size"), size.to_string());
            self.set(&format!("{prefix}-mtime"), mtime.to_string());
        }
    }
    fn write(&self, path: &Path) -> Result<(), String> {
        let mut text = String::from("fabric-receipt 1\n");
        for (k, v) in &self.lines {
            text.push_str(&format!("{k} {v}\n"));
        }
        write_atomic(path, text.as_bytes(), 0o600)
    }
}

// ---- the dry scan ----

/// Every file an agent or a command expansion could bring into a
/// conversation that heddle can list ahead of time: the repo's tracked files,
/// the person's own commands, and installed plugins' commands, skills and
/// agents. Returns (path, rules); never the text.
fn dry_scan(repo: &Repo, home: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if let Some(out) = edge::run(&Program::Git, &repo.main, &["ls-files", "-z"]) {
        for name in out.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            if let Ok(rel) = std::str::from_utf8(name) {
                files.push((rel.to_string(), repo.main.join(rel)));
            }
        }
    }
    let claude = home.join(".claude");
    for (dir, wanted) in [
        (claude.join("commands"), &["/"][..]),
        (
            claude.join("plugins"),
            &["/commands/", "/skills/", "/agents/"][..],
        ),
    ] {
        for path in walk(&dir) {
            let shown = path
                .strip_prefix(home)
                .map_or(path.display().to_string(), |r| format!("~/{}", r.display()));
            if shown.ends_with(".md") && wanted.iter().any(|w| shown.contains(w)) {
                files.push((shown, path));
            }
        }
    }
    files
        .into_iter()
        .filter_map(|(shown, path)| {
            let meta = fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > SCAN_CAP {
                return None;
            }
            let bytes = fs::read(&path).ok()?;
            if bytes.iter().take(8192).any(|b| *b == 0) {
                return None;
            }
            let mut rules: Vec<&str> = detect::detect(&bytes)
                .hits
                .iter()
                .flatten()
                .map(|h| rule_name(h.rule))
                .collect();
            rules.dedup();
            (!rules.is_empty()).then(|| (shown, rules.join(", ")))
        })
        .collect()
}

/// The regular files under `dir`, without following links, bounded.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else {
                continue;
            };
            if t.is_dir() && depth < WALK_DEPTH {
                stack.push((e.path(), depth.saturating_add(1)));
            } else if t.is_file() && out.len() < WALK_FILES {
                out.push(e.path());
            }
        }
    }
    out
}

// ---- setup ----

/// `heddle setup [--mode count]`, run in the project's folder.
pub fn setup(args: &[String]) -> ExitCode {
    let count_only = match args {
        [] => false,
        [flag, mode] if flag == "--mode" && mode == "brake" => false,
        [flag, mode] if flag == "--mode" && mode == "count" => true,
        _ => return fail("usage: heddle setup [--mode count]"),
    };
    match do_setup(count_only) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn do_setup(count_only: bool) -> Result<String, String> {
    let p = Paths::new()?;
    let cwd = std::env::current_dir().map_err(|_| "cannot read the current folder")?;
    let repo = find_repo(&cwd)
        .ok_or("run setup in the folder of a git repository (its main checkout or a worktree)")?;
    if repo.main == p.home {
        return Err(
            "this repository is your home folder; Claude Code keeps its settings elsewhere there"
                .into(),
        );
    }
    let mut out = String::new();
    let findings = dry_scan(&repo, &p.home);
    if !findings.is_empty() {
        out.push_str("The dry scan found text the brake would stop on, in:\n");
        for (path, rules) in &findings {
            out.push_str(&format!("  {path}  ({rules})\n"));
        }
        if !count_only {
            return Err(format!(
                "{out}Not set up. Change those examples to forms that never trip the brake (ids starting SYN-, \
                 Social Security area 000 or 666, \"synthetic\": true), then run setup again."
            ));
        }
    }
    // This binary must start before anything is changed.
    let me = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| "cannot find this heddle binary")?;
    edge::run(&Program::Heddle(me.clone()), &cwd, &["version"])
        .ok_or("this heddle binary does not start")?;
    for (dir, mode) in [(p.fabric.as_path(), 0o700), (p.state.as_path(), 0o700)] {
        mkdirs(dir, mode)?;
    }
    for f in [&p.bin, &p.kept] {
        mkdirs(f.parent().ok_or("no parent folder")?, 0o755)?;
        if fs::canonicalize(f).ok().as_deref() != Some(me.as_path()) {
            copy_atomic(&me, f)?;
        }
    }
    edge::run(&Program::Heddle(p.bin.clone()), &cwd, &["version"])
        .ok_or("the installed heddle does not start")?;
    Machine::open(p.state.clone()).map_err(|e| format!("could not open the state folder: {e}"))?;

    // The settings block, merged into the main checkout's local file.
    let file = repo.main.join(SETTINGS);
    let existing = match fs::read(&file) {
        Ok(bytes) => {
            let backup = tmp_beside(&file).with_file_name("settings.local.json.fabric-backup");
            fs::write(&backup, &bytes).map_err(|e| format!("could not keep a backup: {e}"))?;
            Some(json::from_slice(&bytes).map_err(|_| {
                format!(
                    "{} is not valid JSON. Fix it or move it aside, then run setup again.",
                    file.display()
                )
            })?)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", file.display())),
    };
    let wiring = Wiring {
        heddle: utf8(&p.bin)?,
        root: utf8(&repo.main)?,
        state: utf8(&p.state)?,
        home: utf8(&p.home)?,
        count_only,
    };
    let merged = wiring.merge(existing)?;
    mkdirs(&repo.main.join(".claude"), 0o755)?;
    let mut text = json::to_string_pretty(&merged);
    text.push('\n');
    write_atomic(&file, text.as_bytes(), 0o644)?;
    let reread = fs::read(&file)
        .ok()
        .and_then(|b| json::from_slice(&b).ok())
        .ok_or("the settings file does not read back")?;
    wiring.intact(&reread)?;
    exclude(&repo)?;

    let repair_text = repair_note(&p, &repo.main);
    write_atomic(&p.repair, repair_text.as_bytes(), 0o644)?;
    let mut receipt = Receipt::read(&p.receipt);
    receipt.set("version", VERSION.to_string());
    receipt.set("commit", COMMIT.to_string());
    receipt.record("binary", &p.bin);
    receipt.record("kept", &p.kept);
    receipt.set_repo(&repo.main, count_only);
    receipt.write(&p.receipt)?;

    let mode = if count_only {
        "count only: hits are logged, nothing is ever stopped"
    } else {
        "brake"
    };
    out.push_str(&format!(
        "Fabric is on for this project (heddle {VERSION}, {COMMIT}). Mode: {mode}.\n\
         Changed: {bin}; {settings} (backup kept if it existed).\n\
         This only works in sessions opened on this project's folder.\n\
         Next: run `{bin} doctor --canary` (Claude Code should refuse it), then `{bin} doctor`.\n\n\
         IF CLAUDE EVER REFUSES EVERYTHING IN THIS PROJECT:\n\
         open a new session on your Documents folder and paste:\n  \
         Repair Fabric: read and follow {repair}\n",
        bin = p.bin.display(),
        settings = file.display(),
        repair = p.repair.display(),
    ));
    Ok(out)
}

/// Keep the settings file out of commits: one line in `.git/info/exclude`,
/// unless git already ignores it. No git config is changed.
fn exclude(repo: &Repo) -> Result<(), String> {
    if edge::run(&Program::Git, &repo.main, &["check-ignore", "-q", SETTINGS]).is_some() {
        return Ok(());
    }
    let path = repo.common.join("info/exclude");
    let text = fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == EXCLUDE_LINE) {
        return Ok(());
    }
    mkdirs(&repo.common.join("info"), 0o755)?;
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    write_atomic(
        &path,
        format!("{text}{sep}{EXCLUDE_LINE}\n").as_bytes(),
        0o644,
    )
}

/// `REPAIR.txt`: plain steps that need no working heddle.
fn repair_note(p: &Paths, main: &Path) -> String {
    format!(
        "FABRIC REPAIR (heddle {VERSION})\n\n\
         If Claude refuses everything in a project where Fabric is set up, heddle is\n\
         missing or broken. Work in a session opened on another folder, such as\n\
         Documents. Stop at the first step that works.\n\n\
         1. Put the kept copy back. It checks itself first and needs no network:\n   \
         \"{kept}\" repair\n\n\
         2. If there is no kept copy, install the same version again from\n   \
         https://github.com/xcbarnes/heddle/releases/tag/v{VERSION}\n   \
         the way the setup request describes, and run its `heddle setup` in the project.\n\n\
         3. LAST RESORT. THIS TURNS FABRIC OFF IN THAT PROJECT, THE BRAKE INCLUDED.\n   \
         Move the project's settings file aside. Nothing is lost; the file is kept:\n   \
         mv \"{main}/{SETTINGS}\" \"{main}/{SETTINGS}.fabric-off\"\n",
        kept = p.kept.display(),
        main = main.display(),
    )
}

// ---- doctor ----

/// `heddle doctor [--canary]`.
pub fn doctor(args: &[String]) -> ExitCode {
    match args {
        [] => report(),
        [flag] if flag == "--canary" => canary_ran(),
        _ => fail("usage: heddle doctor [--canary]"),
    }
}

/// The live check's canary. heddle's PreToolUse hook fails on purpose when an
/// agent runs this, so Claude Code should refuse the call. If this runs, it
/// did not.
fn canary_ran() -> ExitCode {
    if let Ok(p) = Paths::new() {
        let _ = fs::write(
            p.state.join("canary-ran"),
            secs(SystemTime::now()).to_string(),
        );
    }
    println!(
        "FAIL OPEN: this Claude Code ran a check that failed on purpose, so it does not block on a broken check."
    );
    ExitCode::from(1)
}

fn report() -> ExitCode {
    let Ok(p) = Paths::new() else {
        return fail("HOME is not set to an absolute folder");
    };
    let receipt = Receipt::read(&p.receipt);
    let mut lines = vec![format!("heddle {VERSION} ({COMMIT})")];
    let mut ok = true;
    let mut check = |good: bool, yes: String, no: String| {
        ok = ok && good;
        lines.push(if good { yes } else { no });
    };
    check(
        receipt.matches("binary", &p.bin),
        format!("Binary: {} is the one setup installed.", p.bin.display()),
        format!(
            "Binary: {} is MISSING or differs from the one setup installed.",
            p.bin.display()
        ),
    );
    check(
        receipt.matches("kept", &p.kept),
        "Kept copy for repair: present.".into(),
        "Kept copy for repair: MISSING or changed.".into(),
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    let repo = find_repo(&cwd);
    let wired = repo
        .as_ref()
        .and_then(|r| receipt.repos().into_iter().find(|(_, m)| *m == r.main));
    match (&repo, wired) {
        (Some(r), Some((count_only, _))) => {
            let file = r.main.join(SETTINGS);
            let wiring = Wiring {
                heddle: p.bin.display().to_string(),
                root: r.main.display().to_string(),
                state: p.state.display().to_string(),
                home: p.home.display().to_string(),
                count_only,
            };
            let parsed = fs::read(&file).ok().and_then(|b| json::from_slice(&b).ok());
            let verdict = parsed
                .ok_or("the file is missing or not valid JSON")
                .and_then(|v| wiring.intact(&v));
            check(
                verdict.is_ok(),
                "Wiring: intact.".into(),
                format!(
                    "Wiring: BROKEN ({}). Run heddle setup here again.",
                    verdict.err().unwrap_or("")
                ),
            );
            let tracked = edge::run(
                &Program::Git,
                &r.main,
                &["ls-files", "--error-unmatch", SETTINGS],
            )
            .is_some();
            check(
                !tracked,
                "Settings file: private (not tracked by git).".into(),
                "Settings file: TRACKED by git. It must stay out of commits.".into(),
            );
            if count_only {
                lines.push(
                    "Live check: does not apply in count mode (nothing is ever blocked).".into(),
                );
            } else {
                let t = |name: &str| {
                    fs::metadata(p.state.join(name))
                        .ok()
                        .and_then(|m| m.modified().ok())
                };
                match (t("canary"), t("canary-ran")) {
                    (_, Some(ran)) if t("canary").is_none_or(|seen| ran >= seen) => check(false, String::new(), "Live check: FAIL OPEN. Claude Code ran a check that failed on purpose; the brake does not hold in this build.".into()),
                    (Some(_), _) => lines.push("Live check: Claude called heddle, and a deliberately broken check was blocked.".into()),
                    (None, _) => check(false, String::new(), format!("Live check: not run yet. Ask your agent to run `{} doctor --canary`; Claude Code should refuse it.", p.bin.display())),
                }
            }
        }
        (Some(_), None) => check(
            false,
            String::new(),
            "This project: not set up. Run heddle setup here.".into(),
        ),
        (None, _) => lines
            .push("This folder is not in a git repository; only the machine checks ran.".into()),
    }
    if p.state.join("config-refused").exists() {
        lines.push(
            "Hook line 9 has refused a settings change that would have removed Fabric's checks from \
             sessions already open; the file on disk changed, and new sessions read it."
                .into(),
        );
    }
    let paused = Machine::open(p.state.clone())
        .map(|m| m.paused().len())
        .unwrap_or(0);
    lines.push(format!("Paused conversations on this computer: {paused}."));
    for l in &lines {
        println!("{l}");
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

// ---- repair ----

/// `heddle repair`, run from the kept copy: it checks itself against the
/// receipt and copies itself back into place. No network.
pub fn repair() -> ExitCode {
    match do_repair() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn do_repair() -> Result<String, String> {
    let p = Paths::new()?;
    let me = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| "cannot find this heddle binary")?;
    if fs::canonicalize(&p.kept).ok().as_deref() != Some(me.as_path()) {
        return Err(format!(
            "only the kept copy repairs. Run: \"{}\" repair",
            p.kept.display()
        ));
    }
    let mut receipt = Receipt::read(&p.receipt);
    if !receipt.matches("kept", &p.kept) {
        return Err(
            "the kept copy differs from the one setup kept. Follow step 2 of REPAIR.txt.".into(),
        );
    }
    mkdirs(p.bin.parent().ok_or("no parent folder")?, 0o755)?;
    copy_atomic(&me, &p.bin)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| p.home.clone());
    edge::run(&Program::Heddle(p.bin.clone()), &cwd, &["version"])
        .ok_or("the repaired heddle does not start")?;
    receipt.record("binary", &p.bin);
    receipt.write(&p.receipt)?;
    Ok(format!(
        "Repaired: {} is back, from the copy setup kept. Start a new session in the project.",
        p.bin.display()
    ))
}

// ---- uninstall ----

/// `heddle uninstall`: takes heddle's entries out of every project's settings
/// file, then removes both binaries and the state folder. It refuses while a
/// paused conversation's transcript still exists.
pub fn uninstall() -> ExitCode {
    match do_uninstall() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn do_uninstall() -> Result<String, String> {
    let p = Paths::new()?;
    if let Ok(m) = Machine::open(p.state.clone()) {
        let live: Vec<String> = m
            .paused()
            .into_iter()
            .filter_map(|(conv, t)| {
                t.filter(|t| t.exists())
                    .map(|t| format!("  {conv}: {}", t.display()))
            })
            .collect();
        if !live.is_empty() {
            return Err(format!(
                "Not uninstalled: these paused conversations still exist.\n{}\nRewind each one past the flagged message, \
                 or delete its transcript (the new-session exit), then run uninstall again.",
                live.join("\n")
            ));
        }
    }
    let receipt = Receipt::read(&p.receipt);
    let mut done = Vec::new();
    for (count_only, main) in receipt.repos() {
        let file = main.join(SETTINGS);
        let Some(v) = fs::read(&file).ok().and_then(|b| json::from_slice(&b).ok()) else {
            done.push(format!(
                "Left alone (missing or not valid JSON): {}",
                file.display()
            ));
            continue;
        };
        let wiring = Wiring {
            heddle: utf8(&p.bin)?,
            root: utf8(&main)?,
            state: utf8(&p.state)?,
            home: utf8(&p.home)?,
            count_only,
        };
        let mut text = json::to_string_pretty(&wiring.remove(v));
        text.push('\n');
        write_atomic(&file, text.as_bytes(), 0o644)?;
        done.push(format!("Removed Fabric's entries from {}", file.display()));
    }
    for f in [&p.bin, &p.kept] {
        if fs::remove_file(f).is_ok() {
            done.push(format!("Removed {}", f.display()));
        }
    }
    if fs::remove_dir_all(&p.fabric).is_ok() {
        done.push(format!("Removed {}", p.fabric.display()));
    }
    done.push("Approvals Claude Code saved in the settings files are kept.".into());
    Ok(done.join("\n"))
}

// ---- phi recheck ----

/// `heddle phi recheck`: the rewind proof, run now for every paused
/// conversation, without a prompt (rung 3). It asserts nothing: a label
/// lowers only when the transcript on disk shows the flagged turn off the
/// live chain. An agent in another session may run it.
pub fn phi(args: &[String]) -> ExitCode {
    if !matches!(args, [a] if a == "recheck") {
        return fail("usage: heddle phi recheck");
    }
    let Ok(p) = Paths::new() else {
        return fail("HOME is not set to an absolute folder");
    };
    let Ok(m) = Machine::open(p.state.clone()) else {
        println!("Not set up on this computer.");
        return ExitCode::SUCCESS;
    };
    let repos = Receipt::read(&p.receipt).repos();
    let paused = m.paused();
    if paused.is_empty() {
        println!("No paused conversations.");
    }
    for (name, transcript) in paused {
        let Some(conv) = fabric_core::ids::ConvId::parse(name.as_bytes()) else {
            continue;
        };
        // Lower both label copies: the second one lives in the repo it ran in.
        let second = repos
            .iter()
            .map(|(_, main)| main.join(".git/fabric/labels"))
            .find(|d| d.join(&name).exists());
        let Ok(m) = Machine::open(p.state.clone()) else {
            continue;
        };
        let m = match second {
            Some(d) => m.with_second(d),
            None => m,
        };
        let proved = transcript
            .as_deref()
            .and_then(|t| fs::read(t).ok())
            .zip(m.rewind_marks(&conv))
            .is_some_and(|(bytes, (start, paused_at))| {
                fabric_core::transcript::newest_entry(&bytes, paused_at)
                    .is_some_and(|leaf| fabric_core::transcript::chain_clear(&bytes, &leaf, start))
            });
        let outcome = match (proved, m.lock()) {
            (true, Ok(_lock)) => {
                let fresh = fabric_core::ids::IncidentId::parse(b"inc-recchk");
                let (mut st, _) = match fresh {
                    Some(f) => m.read_conv(&conv, f),
                    None => continue,
                };
                if let fabric_core::phi::Phi::Exposed { incident, .. } = st.phi {
                    st.phi = fabric_core::phi::phi_step(
                        st.phi,
                        fabric_core::phi::PhiEvent::ChainProvedClear { incident },
                    )
                    .0;
                }
                match m.write_conv(&conv, &st) {
                    Ok(()) if st.phi == fabric_core::phi::Phi::Clean => {
                        "cleared: the rewind is on disk"
                    }
                    Ok(()) | Err(_) => "still paused",
                }
            }
            (true, Err(_)) => "still paused: could not take the lock",
            (false, _) => "still paused: no rewind past the flagged turn found on disk",
        };
        println!("{name}: {outcome}");
    }
    ExitCode::SUCCESS
}

// ---- status ----

/// `heddle status`: the brake's state on this computer. Runs anywhere.
pub fn status() -> ExitCode {
    let Ok(p) = Paths::new() else {
        return fail("HOME is not set to an absolute folder");
    };
    println!("heddle {VERSION} ({COMMIT})");
    let Ok(m) = Machine::open(p.state.clone()) else {
        println!("Not set up on this computer.");
        return ExitCode::SUCCESS;
    };
    let paused = m.paused();
    println!("Paused conversations: {}", paused.len());
    for (conv, t) in paused {
        let where_ = t.map_or("transcript not noted".into(), |t| t.display().to_string());
        println!("  {conv}: {where_}");
    }
    let week = secs(SystemTime::now()).saturating_sub(7 * 24 * 3600);
    let log = fs::read_to_string(p.state.join("counts")).unwrap_or_default();
    let mut counts: Vec<(String, u32)> = Vec::new();
    for l in log.lines() {
        let mut f = l.split(' ');
        let (Some(t), Some(mode), Some(_), Some(rule)) = (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if t.parse::<u64>().is_ok_and(|t| t >= week) {
            let key = format!("{rule} ({mode})");
            match counts.iter_mut().find(|(k, _)| *k == key) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => counts.push((key, 1)),
            }
        }
    }
    counts.sort();
    let shown: Vec<String> = counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
    println!(
        "Hits logged in the last 7 days: {}",
        if shown.is_empty() {
            "none".into()
        } else {
            shown.join(", ")
        }
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("heddle-install-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("scratch");
        fs::canonicalize(&d).expect("canonical")
    }

    #[test]
    fn a_worktree_leads_to_its_main_checkout_without_running_git() {
        let d = scratch("repo");
        fs::create_dir_all(d.join("main/.git/worktrees/wt")).expect("dirs");
        fs::create_dir_all(d.join("wt/sub")).expect("dirs");
        fs::write(
            d.join("wt/.git"),
            format!("gitdir: {}\n", d.join("main/.git/worktrees/wt").display()),
        )
        .expect("file");
        fs::write(d.join("main/.git/worktrees/wt/commondir"), "../..\n").expect("file");
        let r = find_repo(&d.join("wt/sub")).expect("found");
        assert_eq!(r.main, d.join("main"));
        assert_eq!(r.common, d.join("main/.git"));
        let r = find_repo(&d.join("main")).expect("found");
        assert_eq!(r.main, d.join("main"));
        assert!(find_repo(Path::new("/")).is_none());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn the_receipt_round_trips_and_replaces_a_repos_mode() {
        let d = scratch("receipt");
        let path = d.join("receipt");
        let mut r = Receipt::default();
        r.set("version", "0.1.0".into());
        r.set_repo(Path::new("/a"), false);
        r.set_repo(Path::new("/b c"), true);
        r.set_repo(Path::new("/a"), true);
        r.write(&path).expect("write");
        let back = Receipt::read(&path);
        assert_eq!(back.get("version"), Some("0.1.0"));
        assert_eq!(
            back.repos(),
            [(true, PathBuf::from("/b c")), (true, PathBuf::from("/a"))]
        );
        let _ = fs::remove_dir_all(d);
    }
}

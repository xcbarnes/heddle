//! The settings block `setup` writes into the main checkout's
//! `.claude/settings.local.json`, and the one check that it is still there:
//! hook line 9 (ConfigChange) refuses a change that fails it, and `doctor`
//! reports it.
//!
//! The wiring is the enrolment. A brake repo gets nine hook lines (eight
//! here; Stop and SessionEnd come with coordination) with
//! `onFailure: "block"`, four deny rules, `disableAllHooks: false`, and the
//! settings that switch off model calls no hook gates. A count-only repo
//! (decision 4) gets the same hook lines with `--mode count`, no
//! `onFailure`, no ConfigChange line, and `disableAllHooks: false`.

use std::path::Path;

use fabric_core::hook::HookKind;
use fabric_core::json::Value;

/// Telemetry content switches that local settings may turn off (Claude Code
/// honours only these off values from a project or local `env`).
const ENV_OFF: [(&str, &str); 3] = [
    ("OTEL_LOG_USER_PROMPTS", "0"),
    ("OTEL_LOG_TOOL_CONTENT", "0"),
    ("OTEL_LOG_TOOL_DETAILS", "0"),
];
/// Model calls that no hook gates, switched off in a brake repo.
const OFF_KEYS: [&str; 2] = ["promptSuggestionEnabled", "awaySummaryEnabled"];

/// Everything the block names, as the strings it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiring {
    /// The installed binary, absolute.
    pub heddle: String,
    /// The main checkout, absolute.
    pub root: String,
    /// The state folder, absolute.
    pub state: String,
    /// The home folder, for `~/` anchors in deny rules.
    pub home: String,
    /// Decision 4: log, never decide.
    pub count_only: bool,
}

impl Wiring {
    /// The hook lines: event and matcher.
    pub fn lines(&self) -> Vec<(HookKind, Option<&'static str>)> {
        let mut l = vec![
            (HookKind::SessionStart, None),
            (HookKind::UserPromptSubmit, None),
            (HookKind::UserPromptExpansion, None),
            (HookKind::PreToolUse, Some("*")),
            (HookKind::PostToolUse, Some("*")),
            (HookKind::PostToolBatch, None),
            (HookKind::PreCompact, None),
        ];
        if !self.count_only {
            l.push((HookKind::ConfigChange, Some("local_settings")));
        }
        l
    }

    /// One hook line, in exec form.
    pub fn line(&self, kind: HookKind) -> Value {
        let mut args = vec![
            "hook",
            "claude-code",
            kind.name(),
            "--root",
            self.root.as_str(),
            "--state",
            self.state.as_str(),
        ];
        if self.count_only {
            args.extend(["--mode", "count"]);
        }
        let mut o = vec![
            ("type".to_string(), Value::str("command")),
            ("command".to_string(), Value::str(&self.heddle)),
            (
                "args".to_string(),
                Value::Array(args.into_iter().map(Value::str).collect()),
            ),
            ("timeout".to_string(), Value::Number("10".to_string())),
        ];
        if !self.count_only && kind != HookKind::SessionStart {
            o.push(("onFailure".to_string(), Value::str("block")));
        }
        Value::Object(o)
    }

    /// A deny rule's path: `~/…` under the home folder, else `//…`.
    fn rule_path(&self, p: &str) -> String {
        match Path::new(p).strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => format!("/{p}"),
        }
    }

    /// The deny rules: the binary, the folder holding the state, the
    /// settings file, and `/cd`.
    pub fn deny(&self) -> Vec<String> {
        if self.count_only {
            return Vec::new();
        }
        let fabric = Path::new(&self.state)
            .parent()
            .map_or(self.state.clone(), |p| p.display().to_string());
        vec![
            format!("Edit({})", self.rule_path(&self.heddle)),
            format!("Edit({}/**)", self.rule_path(&fabric)),
            format!("Edit(/{}/.claude/settings.local.json)", self.root),
            "Cd".to_string(),
        ]
    }

    /// The settings with this block merged in. Everything else in the file is
    /// kept; earlier heddle lines are replaced.
    pub fn merge(&self, existing: Option<Value>) -> Result<Value, &'static str> {
        let mut top = match existing {
            None => Vec::new(),
            Some(Value::Object(o)) => o,
            Some(_) => return Err("the settings file is not a JSON object"),
        };
        strip_lines(&mut top)?;
        let hooks = child_object(&mut top, "hooks")?;
        for (kind, matcher) in self.lines() {
            let groups = child_array(hooks, kind.name())?;
            let mut g = Vec::new();
            if let Some(m) = matcher {
                g.push(("matcher".to_string(), Value::str(m)));
            }
            g.push(("hooks".to_string(), Value::Array(vec![self.line(kind)])));
            groups.push(Value::Object(g));
        }
        let deny = self.deny();
        if !deny.is_empty() {
            let rules = child_array(child_object(&mut top, "permissions")?, "deny")?;
            for r in deny {
                if !rules.iter().any(|x| x.as_str() == Some(r.as_str())) {
                    rules.push(Value::str(&r));
                }
            }
        }
        set(&mut top, "disableAllHooks", Value::Bool(false));
        if !self.count_only {
            for k in OFF_KEYS {
                set(&mut top, k, Value::Bool(false));
            }
            let env = child_object(&mut top, "env")?;
            for (k, v) in ENV_OFF {
                set(env, k, Value::str(v));
            }
        }
        Ok(Value::Object(top))
    }

    /// The settings with this block taken out, for `uninstall`. Keys that
    /// hold the block's own values go; anything else stays.
    pub fn remove(&self, existing: Value) -> Value {
        let Value::Object(mut top) = existing else {
            return existing;
        };
        let _ = strip_lines(&mut top);
        let deny = self.deny();
        if let Some(Value::Object(perms)) = get_mut(&mut top, "permissions") {
            if let Some(Value::Array(rules)) = get_mut(perms, "deny") {
                rules.retain(|r| !r.as_str().is_some_and(|s| deny.iter().any(|d| d == s)));
            }
            drop_if_empty(perms, "deny");
        }
        drop_if_empty(&mut top, "permissions");
        top.retain(|(k, v)| !(k == "disableAllHooks" && *v == Value::Bool(false)));
        if !self.count_only {
            top.retain(|(k, v)| !(OFF_KEYS.contains(&k.as_str()) && *v == Value::Bool(false)));
            if let Some(Value::Object(env)) = get_mut(&mut top, "env") {
                env.retain(|(k, v)| {
                    !ENV_OFF
                        .iter()
                        .any(|(ek, ev)| k == ek && v.as_str() == Some(ev))
                });
            }
            drop_if_empty(&mut top, "env");
        }
        Value::Object(top)
    }

    /// Whether the settings still carry the whole block. `Err` names the
    /// first piece missing.
    pub fn intact(&self, settings: &Value) -> Result<(), &'static str> {
        self.lines_intact(settings)?;
        let rules = settings
            .get("permissions")
            .and_then(|p| p.get("deny"))
            .and_then(Value::as_array);
        for r in self.deny() {
            if !rules.is_some_and(|rs| rs.iter().any(|x| x.as_str() == Some(r.as_str()))) {
                return Err("a deny rule is missing");
            }
        }
        Ok(())
    }

    /// The part a running session depends on: every hook line, and
    /// `disableAllHooks: false`. Hook line 9 checks this much.
    pub fn lines_intact(&self, settings: &Value) -> Result<(), &'static str> {
        let hooks = settings.get("hooks").ok_or("no hooks")?;
        for (kind, matcher) in self.lines() {
            let line = self.line(kind);
            let found = hooks
                .get(kind.name())
                .and_then(Value::as_array)
                .is_some_and(|groups| {
                    groups.iter().any(|g| {
                        g.get("matcher").and_then(Value::as_str) == matcher
                            && g.get("hooks")
                                .and_then(Value::as_array)
                                .is_some_and(|hs| hs.iter().any(|h| same(h, &line)))
                    })
                });
            if !found {
                return Err("a hook line is missing or changed");
            }
        }
        if settings.get("disableAllHooks") != Some(&Value::Bool(false)) {
            return Err("disableAllHooks is not false");
        }
        Ok(())
    }
}

/// A hook line heddle wrote: a binary named `heddle` run as `hook claude-code`.
fn is_heddle_line(v: &Value) -> bool {
    let named = v
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| Path::new(c).file_name().is_some_and(|f| f == "heddle"));
    let args: Vec<&str> = v
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    named && args.starts_with(&["hook", "claude-code"])
}

/// Take every heddle line out of every event, dropping groups and events
/// that end up empty.
fn strip_lines(top: &mut Vec<(String, Value)>) -> Result<(), &'static str> {
    let Some(hooks) = get_mut(top, "hooks") else {
        return Ok(());
    };
    let Value::Object(events) = hooks else {
        return Err("hooks is not a JSON object");
    };
    for (_, groups) in events.iter_mut() {
        if let Value::Array(gs) = groups {
            for g in gs.iter_mut() {
                if let Value::Object(o) = g
                    && let Some(Value::Array(hs)) = get_mut(o, "hooks")
                {
                    hs.retain(|h| !is_heddle_line(h));
                }
            }
            gs.retain(|g| {
                g.get("hooks")
                    .and_then(Value::as_array)
                    .is_none_or(|hs| !hs.is_empty())
            });
        }
    }
    events.retain(|(_, gs)| gs.as_array().is_none_or(|a| !a.is_empty()));
    drop_if_empty(top, "hooks");
    Ok(())
}

/// Object equality that ignores key order (settings files are re-saved by
/// other tools).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w))
        }
        (Value::Null, _)
        | (Value::Bool(_), _)
        | (Value::Number(_), _)
        | (Value::String(_), _)
        | (Value::Object(_), _)
        | (Value::Array(_), _) => a == b,
    }
}

fn get_mut<'a>(o: &'a mut [(String, Value)], key: &str) -> Option<&'a mut Value> {
    o.iter_mut().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Set `key` to `v`: the first entry is replaced and any duplicates go.
fn set(o: &mut Vec<(String, Value)>, key: &str, v: Value) {
    match o.iter().position(|(k, _)| k == key) {
        Some(i) => {
            if let Some(slot) = o.get_mut(i) {
                slot.1 = v;
            }
            let mut seen = false;
            o.retain(|(k, _)| k != key || !std::mem::replace(&mut seen, true));
        }
        None => o.push((key.to_string(), v)),
    }
}

fn child_object<'a>(
    o: &'a mut Vec<(String, Value)>,
    key: &str,
) -> Result<&'a mut Vec<(String, Value)>, &'static str> {
    if get_mut(o, key).is_none() {
        o.push((key.to_string(), Value::Object(Vec::new())));
    }
    match get_mut(o, key) {
        Some(Value::Object(c)) => Ok(c),
        Some(_) | None => Err("a settings key holds the wrong type"),
    }
}

fn child_array<'a>(
    o: &'a mut Vec<(String, Value)>,
    key: &str,
) -> Result<&'a mut Vec<Value>, &'static str> {
    if get_mut(o, key).is_none() {
        o.push((key.to_string(), Value::Array(Vec::new())));
    }
    match get_mut(o, key) {
        Some(Value::Array(c)) => Ok(c),
        Some(_) | None => Err("a settings key holds the wrong type"),
    }
}

fn drop_if_empty(o: &mut Vec<(String, Value)>, key: &str) {
    o.retain(|(k, v)| {
        k != key
            || !match v {
                Value::Object(x) => x.is_empty(),
                Value::Array(x) => x.is_empty(),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
            }
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use fabric_core::json;

    fn w(count_only: bool) -> Wiring {
        Wiring {
            heddle: "/Users/me/.local/bin/heddle".into(),
            root: "/Users/me/src/repo".into(),
            state: "/Users/me/Library/Application Support/fabric/machine".into(),
            home: "/Users/me".into(),
            count_only,
        }
    }

    #[test]
    fn the_block_merges_keeps_everything_else_and_checks_intact() {
        let mine = json::from_slice(
            br#"{"permissions":{"allow":["Bash(npm test)"],"deny":["Read(./.env)"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/bin/true"}]}]},"model":"x"}"#,
        )
        .expect("json");
        let merged = w(false).merge(Some(mine)).expect("merges");
        assert_eq!(w(false).intact(&merged), Ok(()));
        // Nothing of the person's is lost.
        assert_eq!(merged.get("model").and_then(Value::as_str), Some("x"));
        let deny: Vec<&str> = merged
            .get("permissions")
            .and_then(|p| p.get("deny"))
            .and_then(Value::as_array)
            .expect("deny")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            deny,
            [
                "Read(./.env)",
                "Edit(~/.local/bin/heddle)",
                "Edit(~/Library/Application Support/fabric/**)",
                "Edit(//Users/me/src/repo/.claude/settings.local.json)",
                "Cd"
            ]
        );
        assert!(merged.get("hooks").and_then(|h| h.get("Stop")).is_some());
        // Merging twice changes nothing.
        let again = w(false).merge(Some(merged.clone())).expect("merges");
        assert_eq!(json::to_string(&again), json::to_string(&merged));
        // Taking it out leaves the person's file as it was.
        let out = w(false).remove(merged);
        assert_eq!(
            json::to_string(&out),
            r#"{"permissions":{"allow":["Bash(npm test)"],"deny":["Read(./.env)"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/bin/true"}]}]},"model":"x"}"#
        );
    }

    #[test]
    fn t6_t15_a_change_that_drops_a_line_or_the_off_switch_is_not_intact() {
        let good = w(false).merge(None).expect("merges");
        let text = json::to_string(&good);
        let strip = text.replace(r#""onFailure":"block""#, r#""onFailure":"ignore""#);
        assert!(
            w(false)
                .intact(&json::from_slice(strip.as_bytes()).expect("json"))
                .is_err()
        );
        let off = text.replace(r#""disableAllHooks":false"#, r#""disableAllHooks":true"#);
        assert!(
            w(false)
                .intact(&json::from_slice(off.as_bytes()).expect("json"))
                .is_err()
        );
        assert!(w(false).intact(&Value::Object(Vec::new())).is_err());
        // A different heddle install or state folder is not this block.
        let mut other = w(false);
        other.state = "/tmp/elsewhere".into();
        assert!(other.intact(&good).is_err());
        // Key order does not matter.
        let reordered = text.replace(
            r#"{"type":"command","command":"/Users/me/.local/bin/heddle","#,
            r#"{"command":"/Users/me/.local/bin/heddle","type":"command","#,
        );
        assert_eq!(
            w(false).intact(&json::from_slice(reordered.as_bytes()).expect("json")),
            Ok(())
        );
    }

    #[test]
    fn decision_4_count_lines_carry_the_mode_and_never_block() {
        let c = w(true).merge(None).expect("merges");
        assert_eq!(w(true).intact(&c), Ok(()));
        let text = json::to_string(&c);
        assert!(
            !text.contains("onFailure") && !text.contains("ConfigChange") && !text.contains("deny")
        );
        assert!(text.contains(r#""--mode","count""#));
        // The brake block is not intact as a count block, nor the other way.
        assert!(w(false).intact(&c).is_err());
        assert!(
            w(true)
                .intact(&w(false).merge(None).expect("merges"))
                .is_err()
        );
    }
}

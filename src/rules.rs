//! Compiled rules and their application to arguments / environment.

use std::ffi::{OsStr, OsString};

use regex::{Captures, Regex};

use crate::config::{
    scalar_list, Action, ArgScope, Fallback, IfExists, MatchOn, NamedPosition, On, PositionDef,
    RuleDef, WhenDef,
};
use crate::pattern::{expand, Pattern};

/// Environment as an ordered list of `(name, value)` pairs.
pub type EnvList = Vec<(OsString, OsString)>;

/// Environment variable names are case-insensitive on Windows.
pub const ENV_CASE_INSENSITIVE: bool = cfg!(windows);

#[cfg(windows)]
const DEFAULT_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const DEFAULT_SEPARATOR: &str = ":";

/// Arguments (without argv[0]) and environment that will be passed on.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub args: Vec<OsString>,
    pub env: EnvList,
}

pub struct Rule {
    pub name: String,
    when: Option<When>,
    op: Op,
}

enum Edit {
    /// Replace with template(s) expanded against the `match` captures.
    Template(Vec<String>),
    /// `regex.replace_all(subject, replacement)`.
    Regex(Regex, String),
}

enum Position {
    Index(i64),
    Before(Pattern),
    After(Pattern),
}

enum Op {
    EnvAdd {
        key: OsString,
        value: String,
        if_exists: IfExists,
        separator: String,
    },
    EnvRemove {
        pattern: Pattern,
    },
    EnvReplace {
        pattern: Pattern,
        edit: Option<Edit>,
        rename: Option<String>,
    },
    ArgAdd {
        values: Vec<String>,
        position: Position,
        fallback: Fallback,
        match_on: MatchOn,
    },
    ArgRemove {
        pattern: Pattern,
        match_on: MatchOn,
        with_value: bool,
    },
    ArgReplace {
        pattern: Pattern,
        match_on: MatchOn,
        scope: ArgScope,
        edit: Edit,
    },
}

struct When {
    os: Vec<String>,
    env_set: Vec<Pattern>,
    env_unset: Vec<Pattern>,
    env_match: Vec<(Pattern, Pattern)>,
    arg_present: Vec<Pattern>,
    arg_absent: Vec<Pattern>,
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

pub fn compile(defs: &[RuleDef]) -> Result<Vec<Rule>, String> {
    let mut out = Vec::with_capacity(defs.len());
    for (i, def) in defs.iter().enumerate() {
        let name = def.name.clone().unwrap_or_else(|| format!("#{}", i + 1));
        if !def.enabled {
            continue;
        }
        let rule =
            compile_one(def, name.clone()).map_err(|e| format!("rule #{} ({name}): {e}", i + 1))?;
        out.push(rule);
    }
    Ok(out)
}

fn forbid(fields: &[(&str, bool)], context: &str) -> Result<(), String> {
    for (name, present) in fields {
        if *present {
            return Err(format!("field `{name}` is not allowed for {context}"));
        }
    }
    Ok(())
}

fn compile_one(d: &RuleDef, name: String) -> Result<Rule, String> {
    let ci = d.on == On::Env && ENV_CASE_INSENSITIVE;
    let pattern = || -> Result<Pattern, String> {
        let src = d.pattern.as_deref().ok_or("`match` is required")?;
        Pattern::new(src, ci)
    };
    let edit = || -> Result<Option<Edit>, String> {
        match (&d.value, &d.regex) {
            (Some(_), Some(_)) => Err("`value` and `regex` are mutually exclusive".into()),
            (Some(v), None) => {
                if d.replacement.is_some() {
                    return Err("`replacement` requires `regex`".into());
                }
                Ok(Some(Edit::Template(scalar_list(v)?)))
            }
            (None, Some(r)) => {
                let re = Regex::new(r).map_err(|e| format!("invalid `regex`: {e}"))?;
                Ok(Some(Edit::Regex(
                    re,
                    d.replacement.clone().unwrap_or_default(),
                )))
            }
            (None, None) => Ok(None),
        }
    };
    let match_on = d.match_on.unwrap_or(MatchOn::Key);
    let ctx = format!("on: {:?} / action: {:?}", d.on, d.action).to_lowercase();

    let op = match (d.on, d.action) {
        (On::Env, Action::Add) => {
            forbid(
                &[
                    ("match", d.pattern.is_some()),
                    ("position", d.position.is_some()),
                    ("fallback", d.fallback.is_some()),
                    ("regex", d.regex.is_some()),
                    ("replacement", d.replacement.is_some()),
                    ("rename", d.rename.is_some()),
                    ("scope", d.scope.is_some()),
                    ("match_on", d.match_on.is_some()),
                    ("with_value", d.with_value.is_some()),
                ],
                &ctx,
            )?;
            let key = d.key.clone().ok_or("`key` is required")?;
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(format!("invalid variable name {key:?}"));
            }
            let separator = d
                .separator
                .clone()
                .unwrap_or_else(|| DEFAULT_SEPARATOR.to_string());
            let parts = scalar_list(d.value.as_ref().ok_or("`value` is required")?)?;
            Op::EnvAdd {
                key: key.into(),
                value: parts.join(&separator),
                if_exists: d.if_exists.unwrap_or(IfExists::Overwrite),
                separator,
            }
        }
        (On::Env, Action::Remove) => {
            forbid(
                &[
                    ("key", d.key.is_some()),
                    ("value", d.value.is_some()),
                    ("position", d.position.is_some()),
                    ("fallback", d.fallback.is_some()),
                    ("if_exists", d.if_exists.is_some()),
                    ("separator", d.separator.is_some()),
                    ("regex", d.regex.is_some()),
                    ("replacement", d.replacement.is_some()),
                    ("rename", d.rename.is_some()),
                    ("scope", d.scope.is_some()),
                    ("match_on", d.match_on.is_some()),
                    ("with_value", d.with_value.is_some()),
                ],
                &ctx,
            )?;
            Op::EnvRemove {
                pattern: pattern()?,
            }
        }
        (On::Env, Action::Replace) => {
            forbid(
                &[
                    ("key", d.key.is_some()),
                    ("position", d.position.is_some()),
                    ("fallback", d.fallback.is_some()),
                    ("if_exists", d.if_exists.is_some()),
                    ("separator", d.separator.is_some()),
                    ("scope", d.scope.is_some()),
                    ("match_on", d.match_on.is_some()),
                    ("with_value", d.with_value.is_some()),
                ],
                &ctx,
            )?;
            let edit = edit()?;
            if let Some(Edit::Template(t)) = &edit {
                if t.len() != 1 {
                    return Err("`value` must be a single string for env replace".into());
                }
            }
            if edit.is_none() && d.rename.is_none() {
                return Err("one of `value`, `regex` or `rename` is required".into());
            }
            Op::EnvReplace {
                pattern: pattern()?,
                edit,
                rename: d.rename.clone(),
            }
        }
        (On::Arg, Action::Add) => {
            forbid(
                &[
                    ("match", d.pattern.is_some()),
                    ("key", d.key.is_some()),
                    ("if_exists", d.if_exists.is_some()),
                    ("separator", d.separator.is_some()),
                    ("regex", d.regex.is_some()),
                    ("replacement", d.replacement.is_some()),
                    ("rename", d.rename.is_some()),
                    ("scope", d.scope.is_some()),
                    ("with_value", d.with_value.is_some()),
                ],
                &ctx,
            )?;
            let values = scalar_list(d.value.as_ref().ok_or("`value` is required")?)?;
            let position = match d
                .position
                .clone()
                .unwrap_or(PositionDef::Named(NamedPosition::End))
            {
                PositionDef::Index(i) => Position::Index(i),
                PositionDef::Named(NamedPosition::Start) => Position::Index(0),
                PositionDef::Named(NamedPosition::End) => Position::Index(-1),
                PositionDef::Before { before } => Position::Before(Pattern::new(&before, false)?),
                PositionDef::After { after } => Position::After(Pattern::new(&after, false)?),
            };
            Op::ArgAdd {
                values,
                position,
                fallback: d.fallback.unwrap_or(Fallback::End),
                match_on,
            }
        }
        (On::Arg, Action::Remove) => {
            forbid(
                &[
                    ("key", d.key.is_some()),
                    ("value", d.value.is_some()),
                    ("position", d.position.is_some()),
                    ("fallback", d.fallback.is_some()),
                    ("if_exists", d.if_exists.is_some()),
                    ("separator", d.separator.is_some()),
                    ("regex", d.regex.is_some()),
                    ("replacement", d.replacement.is_some()),
                    ("rename", d.rename.is_some()),
                    ("scope", d.scope.is_some()),
                ],
                &ctx,
            )?;
            Op::ArgRemove {
                pattern: pattern()?,
                match_on,
                with_value: d.with_value.unwrap_or(false),
            }
        }
        (On::Arg, Action::Replace) => {
            forbid(
                &[
                    ("key", d.key.is_some()),
                    ("position", d.position.is_some()),
                    ("fallback", d.fallback.is_some()),
                    ("if_exists", d.if_exists.is_some()),
                    ("separator", d.separator.is_some()),
                    ("rename", d.rename.is_some()),
                    ("with_value", d.with_value.is_some()),
                ],
                &ctx,
            )?;
            let scope = d.scope.unwrap_or(ArgScope::Arg);
            let edit = edit()?.ok_or("one of `value` or `regex` is required")?;
            if let Edit::Template(t) = &edit {
                if scope == ArgScope::Value && t.len() != 1 {
                    return Err("`value` must be a single string when `scope: value`".into());
                }
            }
            Op::ArgReplace {
                pattern: pattern()?,
                match_on,
                scope,
                edit,
            }
        }
    };

    let when = d.when.as_ref().map(compile_when).transpose()?;
    Ok(Rule { name, when, op })
}

fn compile_when(w: &WhenDef) -> Result<When, String> {
    let env_pats = |v: &[String]| -> Result<Vec<Pattern>, String> {
        v.iter()
            .map(|p| Pattern::new(p, ENV_CASE_INSENSITIVE))
            .collect()
    };
    let arg_pats = |v: &[String]| -> Result<Vec<Pattern>, String> {
        v.iter().map(|p| Pattern::new(p, false)).collect()
    };
    Ok(When {
        os: w.os.iter().map(|s| s.to_ascii_lowercase()).collect(),
        env_set: env_pats(&w.env_set)?,
        env_unset: env_pats(&w.env_unset)?,
        env_match: w
            .env_match
            .iter()
            .map(|(k, v)| {
                Ok((
                    Pattern::new(k, ENV_CASE_INSENSITIVE)?,
                    Pattern::new(v, false)?,
                ))
            })
            .collect::<Result<_, String>>()?,
        arg_present: arg_pats(&w.arg_present)?,
        arg_absent: arg_pats(&w.arg_absent)?,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `--name=value` -> `--name`; anything else is returned unchanged.
fn arg_key(a: &str) -> &str {
    if a.starts_with('-') {
        if let Some(i) = a.find('=') {
            return &a[..i];
        }
    }
    a
}

fn has_inline_value(a: &str) -> bool {
    a.starts_with('-') && a.contains('=')
}

fn subject(a: &str, m: MatchOn) -> &str {
    match m {
        MatchOn::Key => arg_key(a),
        MatchOn::Full => a,
    }
}

fn arg_matches_any_form(p: &Pattern, a: &OsStr) -> bool {
    a.to_str()
        .is_some_and(|s| p.is_match(s) || p.is_match(arg_key(s)))
}

fn env_key_eq(a: &OsStr, b: &OsStr) -> bool {
    if ENV_CASE_INSENSITIVE {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

pub fn env_get<'a>(env: &'a EnvList, key: &OsStr) -> Option<&'a OsString> {
    env.iter().find(|(k, _)| env_key_eq(k, key)).map(|(_, v)| v)
}

/// Set a variable, keeping the existing spelling of the name on Windows.
pub fn env_set(env: &mut EnvList, key: OsString, value: OsString) {
    match env.iter().position(|(k, _)| env_key_eq(k, &key)) {
        Some(i) => {
            env[i].1 = value;
            let mut j = i + 1;
            while j < env.len() {
                if env_key_eq(&env[j].0, &key) {
                    env.remove(j);
                } else {
                    j += 1;
                }
            }
        }
        None => env.push((key, value)),
    }
}

pub fn env_remove(env: &mut EnvList, key: &OsStr) {
    env.retain(|(k, _)| !env_key_eq(k, key));
}

fn apply_edit(edit: &Edit, subject: &str, caps: &Captures<'_>) -> Vec<String> {
    match edit {
        Edit::Template(t) => t.iter().map(|tpl| expand(caps, tpl)).collect(),
        Edit::Regex(re, rep) => vec![re.replace_all(subject, rep.as_str()).into_owned()],
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

impl When {
    fn eval(&self, st: &State) -> bool {
        let env_name_matches = |p: &Pattern| {
            st.env
                .iter()
                .any(|(k, _)| k.to_str().is_some_and(|k| p.is_match(k)))
        };
        let arg_matches = |p: &Pattern| st.args.iter().any(|a| arg_matches_any_form(p, a));

        let os_ok = self.os.is_empty()
            || self
                .os
                .iter()
                .any(|o| o == std::env::consts::OS || o == std::env::consts::FAMILY);

        os_ok
            && self.env_set.iter().all(env_name_matches)
            && !self.env_unset.iter().any(env_name_matches)
            && self.env_match.iter().all(|(np, vp)| {
                st.env.iter().any(|(k, v)| {
                    k.to_str().is_some_and(|k| np.is_match(k))
                        && v.to_str().is_some_and(|v| vp.is_match(v))
                })
            })
            && self.arg_present.iter().all(arg_matches)
            && !self.arg_absent.iter().any(arg_matches)
    }
}

impl Op {
    /// Apply the operation; returns `true` if anything changed.
    fn apply(&self, st: &mut State) -> bool {
        match self {
            Op::EnvAdd {
                key,
                value,
                if_exists,
                separator,
            } => {
                let new = match (env_get(&st.env, key), if_exists) {
                    (None, _) | (Some(_), IfExists::Overwrite) => Some(OsString::from(value)),
                    (Some(_), IfExists::Skip) => None,
                    (Some(old), IfExists::Append) => {
                        let mut s = old.clone();
                        if !s.is_empty() {
                            s.push(separator);
                        }
                        s.push(value);
                        Some(s)
                    }
                    (Some(old), IfExists::Prepend) => {
                        let mut s = OsString::from(value);
                        if !old.is_empty() {
                            s.push(separator);
                            s.push(old);
                        }
                        Some(s)
                    }
                };
                match new {
                    Some(v) => {
                        env_set(&mut st.env, key.clone(), v);
                        true
                    }
                    None => false,
                }
            }

            Op::EnvRemove { pattern } => {
                let before = st.env.len();
                st.env
                    .retain(|(k, _)| !k.to_str().is_some_and(|k| pattern.is_match(k)));
                before != st.env.len()
            }

            Op::EnvReplace {
                pattern,
                edit,
                rename,
            } => {
                let mut changes: Vec<(OsString, OsString, OsString)> = Vec::new();
                for (k, v) in &st.env {
                    let Some(ks) = k.to_str() else { continue };
                    let Some(caps) = pattern.captures(ks) else {
                        continue;
                    };
                    let new_v: OsString = match (edit, v.to_str()) {
                        (None, _) | (Some(Edit::Regex(..)), None) => v.clone(),
                        (Some(e), Some(vs)) => apply_edit(e, vs, &caps).concat().into(),
                        (Some(e @ Edit::Template(_)), None) => {
                            apply_edit(e, "", &caps).concat().into()
                        }
                    };
                    let new_k: OsString = match rename {
                        Some(tpl) => {
                            let n = expand(&caps, tpl);
                            if n.is_empty() || n.contains('=') || n.contains('\0') {
                                k.clone()
                            } else {
                                n.into()
                            }
                        }
                        None => k.clone(),
                    };
                    changes.push((k.clone(), new_k, new_v));
                }
                let changed = changes
                    .iter()
                    .any(|(ok, nk, nv)| ok != nk || env_get(&st.env, ok) != Some(nv));
                for (old_k, _, _) in &changes {
                    env_remove(&mut st.env, old_k);
                }
                for (_, new_k, new_v) in changes {
                    env_set(&mut st.env, new_k, new_v);
                }
                changed
            }

            Op::ArgAdd {
                values,
                position,
                fallback,
                match_on,
            } => {
                let n = st.args.len();
                let find = |p: &Pattern| {
                    st.args.iter().position(|a| {
                        a.to_str()
                            .is_some_and(|s| p.is_match(subject(s, *match_on)))
                    })
                };
                let fb = || match fallback {
                    Fallback::Start => Some(0),
                    Fallback::End => Some(n),
                    Fallback::Skip => None,
                };
                let idx = match position {
                    Position::Index(i) if *i >= 0 => Some((*i as usize).min(n)),
                    Position::Index(i) => Some((n as i64 + 1 + *i).max(0) as usize),
                    Position::Before(p) => find(p).or_else(fb),
                    Position::After(p) => find(p).map(|j| j + 1).or_else(fb),
                };
                match idx {
                    Some(idx) => {
                        st.args.splice(idx..idx, values.iter().map(OsString::from));
                        true
                    }
                    None => false,
                }
            }

            Op::ArgRemove {
                pattern,
                match_on,
                with_value,
            } => {
                let args = std::mem::take(&mut st.args);
                let mut out = Vec::with_capacity(args.len());
                let mut changed = false;
                let mut i = 0;
                while i < args.len() {
                    if let Some(s) = args[i].to_str() {
                        if pattern.is_match(subject(s, *match_on)) {
                            changed = true;
                            i += if *with_value && !has_inline_value(s) && i + 1 < args.len() {
                                2
                            } else {
                                1
                            };
                            continue;
                        }
                    }
                    out.push(args[i].clone());
                    i += 1;
                }
                st.args = out;
                changed
            }

            Op::ArgReplace {
                pattern,
                match_on,
                scope,
                edit,
            } => {
                let args = std::mem::take(&mut st.args);
                let mut out: Vec<OsString> = Vec::with_capacity(args.len());
                let mut changed = false;
                let mut i = 0;
                while i < args.len() {
                    let caps = args[i]
                        .to_str()
                        .and_then(|s| pattern.captures(subject(s, *match_on)).map(|c| (s, c)));
                    let Some((s, caps)) = caps else {
                        out.push(args[i].clone());
                        i += 1;
                        continue;
                    };
                    match scope {
                        ArgScope::Arg => {
                            let new = apply_edit(edit, s, &caps);
                            changed |= new.len() != 1 || new[0] != s;
                            out.extend(new.into_iter().map(OsString::from));
                            i += 1;
                        }
                        ArgScope::Value if has_inline_value(s) => {
                            let (k, v) = s.split_once('=').expect("has '='");
                            let nv = apply_edit(edit, v, &caps).concat();
                            changed |= nv != v;
                            out.push(format!("{k}={nv}").into());
                            i += 1;
                        }
                        ArgScope::Value if i + 1 < args.len() => {
                            out.push(args[i].clone());
                            match args[i + 1].to_str() {
                                Some(v) => {
                                    let nv = apply_edit(edit, v, &caps).concat();
                                    changed |= nv != v;
                                    out.push(nv.into());
                                }
                                None => out.push(args[i + 1].clone()),
                            }
                            i += 2;
                        }
                        ArgScope::Value => {
                            out.push(args[i].clone());
                            i += 1;
                        }
                    }
                }
                st.args = out;
                changed
            }
        }
    }
}

/// Apply rules in order. Returns human-readable trace lines.
pub fn apply(rules: &[Rule], st: &mut State, disabled: &[String]) -> Vec<String> {
    let mut trace = Vec::new();
    for r in rules {
        if disabled.iter().any(|d| d == &r.name) {
            trace.push(format!("rule {}: disabled via environment", r.name));
            continue;
        }
        if let Some(w) = &r.when {
            if !w.eval(st) {
                trace.push(format!("rule {}: condition not met", r.name));
                continue;
            }
        }
        let changed = r.op.apply(st);
        trace.push(format!(
            "rule {}: {}",
            r.name,
            if changed { "applied" } else { "no match" }
        ));
    }
    trace
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_str;

    fn run(rules_yaml: &str, args: &[&str], env: &[(&str, &str)]) -> State {
        let cfg = parse_str(&format!("target: x\nrules:\n{rules_yaml}")).unwrap();
        let rules = compile(&cfg.rules).unwrap();
        let mut st = State {
            args: args.iter().map(OsString::from).collect(),
            env: env.iter().map(|(k, v)| (k.into(), v.into())).collect(),
        };
        apply(&rules, &mut st, &[]);
        st
    }

    fn args(st: &State) -> Vec<&str> {
        st.args.iter().map(|a| a.to_str().unwrap()).collect()
    }

    fn env(st: &State, k: &str) -> Option<String> {
        env_get(&st.env, OsStr::new(k)).map(|v| v.to_string_lossy().into_owned())
    }

    #[test]
    fn arg_add_positions() {
        let st = run(
            "  - { on: arg, action: add, value: X, position: start }",
            &["a", "b"],
            &[],
        );
        assert_eq!(args(&st), ["X", "a", "b"]);
        let st = run("  - { on: arg, action: add, value: X }", &["a", "b"], &[]);
        assert_eq!(args(&st), ["a", "b", "X"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: 1 }",
            &["a", "b"],
            &[],
        );
        assert_eq!(args(&st), ["a", "X", "b"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: -2 }",
            &["a", "b"],
            &[],
        );
        assert_eq!(args(&st), ["a", "X", "b"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: 99 }",
            &["a"],
            &[],
        );
        assert_eq!(args(&st), ["a", "X"]);
        let st = run(
            "  - { on: arg, action: add, value: [--x, '1'], position: { before: '--' } }",
            &["a", "--", "b"],
            &[],
        );
        assert_eq!(args(&st), ["a", "--x", "1", "--", "b"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: { before: '--' } }",
            &["a"],
            &[],
        );
        assert_eq!(args(&st), ["a", "X"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: { after: run }, fallback: skip }",
            &["a"],
            &[],
        );
        assert_eq!(args(&st), ["a"]);
        let st = run(
            "  - { on: arg, action: add, value: X, position: { after: run } }",
            &["run", "a"],
            &[],
        );
        assert_eq!(args(&st), ["run", "X", "a"]);
    }

    #[test]
    fn arg_remove() {
        let st = run(
            "  - { on: arg, action: remove, match: '--token', with_value: true }",
            &["--token", "s3cr3t", "a", "--token=x", "b"],
            &[],
        );
        assert_eq!(args(&st), ["a", "b"]);
        let st = run(
            "  - { on: arg, action: remove, match: '--debug*' }",
            &["--debug", "--debug-x", "c"],
            &[],
        );
        assert_eq!(args(&st), ["c"]);
        let st = run(
            "  - { on: arg, action: remove, match: '--a=1', match_on: full }",
            &["--a=1", "--a=2"],
            &[],
        );
        assert_eq!(args(&st), ["--a=2"]);
    }

    #[test]
    fn arg_replace() {
        // whole-argument template with glob captures
        let st = run(
            "  - { on: arg, action: replace, match: '--old-*', value: '--new-${1}' }",
            &["--old-flag=1", "--old-x"],
            &[],
        );
        assert_eq!(args(&st), ["--new-flag", "--new-x"]);
        // regex substitution on the whole argument
        let st = run(
            r#"  - { on: arg, action: replace, match: '--url', regex: 'http://(.+)', replacement: 'https://$1' }"#,
            &["--url=http://a.b", "--url", "http://c"],
            &[],
        );
        assert_eq!(args(&st), ["--url=https://a.b", "--url", "http://c"]);
        // value scope: inline and separate forms
        let st = run(
            r#"  - { on: arg, action: replace, match: '--level', scope: value, regex: '(?i)^debug$', replacement: info }"#,
            &["--level=DEBUG", "--level", "debug", "x"],
            &[],
        );
        assert_eq!(args(&st), ["--level=info", "--level", "info", "x"]);
        // one argument split into several
        let st = run(
            "  - { on: arg, action: replace, match: '-v', value: ['--verbose', '--color'] }",
            &["-v"],
            &[],
        );
        assert_eq!(args(&st), ["--verbose", "--color"]);
    }

    #[test]
    fn env_ops() {
        let st = run(
            "  - { on: env, action: remove, match: 'REALM_NAME_*' }",
            &[],
            &[("REALM_NAME_A", "1"), ("REALM_NAME_B", "2"), ("KEEP", "3")],
        );
        assert_eq!(st.env.len(), 1);

        let st = run(
            "  - { on: env, action: add, key: PATH, value: /opt/bin, if_exists: prepend, separator: ':' }",
            &[],
            &[("PATH", "/usr/bin")],
        );
        assert_eq!(env(&st, "PATH").unwrap(), "/opt/bin:/usr/bin");

        let st = run(
            "  - { on: env, action: add, key: A, value: new, if_exists: skip }",
            &[],
            &[("A", "old")],
        );
        assert_eq!(env(&st, "A").unwrap(), "old");

        let st = run(
            "  - { on: env, action: replace, match: 'REALM_NAME_*', rename: 'APP_REALM_${1}', regex: '^dev-(.*)$', replacement: 'prod-$1' }",
            &[],
            &[("REALM_NAME_X", "dev-eu"), ("REALM_NAME_Y", "other")],
        );
        assert_eq!(env(&st, "APP_REALM_X").unwrap(), "prod-eu");
        assert_eq!(env(&st, "APP_REALM_Y").unwrap(), "other");
        assert!(env(&st, "REALM_NAME_X").is_none());
    }

    #[test]
    fn conditions() {
        let yaml = "  - on: arg\n    action: add\n    value: --ci\n    when: { env_set: CI, arg_absent: '--ci' }\n";
        assert_eq!(args(&run(yaml, &["a"], &[("CI", "1")])), ["a", "--ci"]);
        assert_eq!(args(&run(yaml, &["a"], &[])), ["a"]);
        assert_eq!(args(&run(yaml, &["--ci"], &[("CI", "1")])), ["--ci"]);

        let yaml = "  - on: env\n    action: add\n    key: MODE\n    value: prod\n    when:\n      env_match: { STAGE: 're:^prod' }\n";
        assert_eq!(
            env(&run(yaml, &[], &[("STAGE", "production")]), "MODE").unwrap(),
            "prod"
        );
        assert!(env(&run(yaml, &[], &[("STAGE", "dev")]), "MODE").is_none());
    }

    #[test]
    fn validation_errors() {
        let bad = [
            "  - { on: env, action: add, value: x }",
            "  - { on: env, action: remove }",
            "  - { on: arg, action: replace, match: a }",
            "  - { on: arg, action: add, value: x, match: a }",
            "  - { on: arg, action: replace, match: a, value: b, regex: c }",
            "  - { on: arg, action: replace, match: 're:[', value: b }",
        ];
        for b in bad {
            let cfg = parse_str(&format!("target: x\nrules:\n{b}")).unwrap();
            assert!(compile(&cfg.rules).is_err(), "should fail: {b}");
        }
    }
}

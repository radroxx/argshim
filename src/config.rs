//! Configuration file schema, discovery, `${env:VAR}` interpolation and
//! `ARGSHIM_*` environment overrides.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Deserializer};
use serde_yaml_ng::Value;

use crate::Failure;

/// Prefix of the wrapper's own environment variables.
pub const ENV_PREFIX: &str = "ARGSHIM_";
/// Name of the shared config file looked up next to the executable.
const DEFAULT_CONFIG_NAME: &str = "argshim";
/// Exit code used for configuration problems.
pub const EXIT_CONFIG: i32 = 125;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Path or bare name (looked up in PATH) of the real executable.
    pub target: Option<String>,
    /// Pass the wrapper's original argv[0] to the target (Unix only).
    #[serde(default)]
    pub preserve_argv0: bool,
    /// Remove `ARGSHIM_*` variables from the target's environment.
    #[serde(default = "default_true")]
    pub strip_own_env: bool,
    /// Append diagnostic output to this file (never touches stdout/stderr).
    #[serde(default)]
    pub log_file: Option<String>,
    #[serde(default)]
    pub rules: Vec<RuleDef>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target: None,
            preserve_argv0: false,
            strip_own_env: true,
            log_file: None,
            rules: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum On {
    #[serde(alias = "envs")]
    Env,
    #[serde(alias = "args")]
    Arg,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    #[serde(alias = "set")]
    Add,
    #[serde(alias = "delete")]
    Remove,
    Replace,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IfExists {
    Overwrite,
    Skip,
    Append,
    Prepend,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MatchOn {
    /// For `-x=..`/`--x=..` arguments only the part before `=` is matched.
    Key,
    /// The whole argument is matched.
    Full,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArgScope {
    /// Replace the matched argument itself.
    Arg,
    /// Replace the option's value: text after `=` or the next argument.
    Value,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NamedPosition {
    Start,
    End,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    Start,
    End,
    Skip,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum PositionDef {
    Index(i64),
    Named(NamedPosition),
    Before { before: String },
    After { after: String },
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WhenDef {
    /// `linux`, `windows`, `macos`, `unix`, ... (any of)
    #[serde(default, deserialize_with = "one_or_many")]
    pub os: Vec<String>,
    /// Every pattern must match at least one variable name.
    #[serde(default, deserialize_with = "one_or_many")]
    pub env_set: Vec<String>,
    /// No variable name may match any of these patterns.
    #[serde(default, deserialize_with = "one_or_many")]
    pub env_unset: Vec<String>,
    /// name pattern -> value pattern; every entry must match some variable.
    #[serde(default)]
    pub env_match: BTreeMap<String, String>,
    /// Every pattern must match at least one argument.
    #[serde(default, deserialize_with = "one_or_many")]
    pub arg_present: Vec<String>,
    /// No argument may match any of these patterns.
    #[serde(default, deserialize_with = "one_or_many")]
    pub arg_absent: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    /// Optional label used in logs and in `ARGSHIM_DISABLE_RULES`.
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub on: On,
    pub action: Action,
    /// Pattern selecting env variable names / arguments.
    #[serde(rename = "match")]
    pub pattern: Option<String>,
    /// Exact variable name (env add).
    pub key: Option<String>,
    /// Value(s) to add, or replacement template(s).
    pub value: Option<Value>,
    pub position: Option<PositionDef>,
    pub fallback: Option<Fallback>,
    pub if_exists: Option<IfExists>,
    pub separator: Option<String>,
    pub regex: Option<String>,
    pub replacement: Option<String>,
    pub rename: Option<String>,
    pub scope: Option<ArgScope>,
    pub match_on: Option<MatchOn>,
    pub with_value: Option<bool>,
    pub when: Option<WhenDef>,
}

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// Turn a YAML scalar or list of scalars into strings.
pub fn scalar_list(v: &Value) -> Result<Vec<String>, String> {
    fn scalar(v: &Value) -> Result<String, String> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Null => Ok(String::new()),
            _ => Err("`value` must be a scalar or a list of scalars".into()),
        }
    }
    match v {
        Value::Sequence(seq) => seq.iter().map(scalar).collect(),
        other => Ok(vec![scalar(other)?]),
    }
}

// ---------------------------------------------------------------------------
// Runtime overrides from the environment
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Runtime {
    pub config_path: Option<PathBuf>,
    pub target: Option<String>,
    pub log_file: Option<String>,
    pub preserve_argv0: Option<bool>,
    pub disable: bool,
    pub debug: bool,
    pub dry_run: bool,
    pub disabled_rules: Vec<String>,
}

fn env_str(name: &str) -> Option<String> {
    std::env::var(format!("{ENV_PREFIX}{name}"))
        .ok()
        .filter(|s| !s.is_empty())
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl Runtime {
    pub fn from_env() -> Self {
        Self {
            config_path: env_str("CONFIG").map(PathBuf::from),
            target: env_str("TARGET"),
            log_file: env_str("LOG_FILE"),
            preserve_argv0: env_str("PRESERVE_ARGV0").map(|s| truthy(&s)),
            disable: env_str("DISABLE").is_some_and(|s| truthy(&s)),
            debug: env_str("DEBUG").is_some_and(|s| truthy(&s)),
            dry_run: env_str("DRY_RUN").is_some_and(|s| truthy(&s)),
            disabled_rules: env_str("DISABLE_RULES")
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub struct Loaded {
    pub config: Config,
    /// Directory relative target paths are resolved against.
    pub base_dir: PathBuf,
    pub path: Option<PathBuf>,
}

fn cfg_err(msg: impl Into<String>) -> Failure {
    Failure {
        code: EXIT_CONFIG,
        msg: msg.into(),
    }
}

/// Directory the wrapper was invoked from, *without* resolving symlinks.
///
/// `current_exe()` follows symlinks on Linux, so for a shim like
/// `/usr/local/bin/terraform -> /opt/argshim/argshim` started as plain
/// `terraform`, the directory is recovered by repeating the shell's PATH
/// lookup of argv[0] and picking the entry that points at this executable.
fn invoked_dir(argv0: &Path) -> Option<PathBuf> {
    if argv0.components().count() > 1 {
        return argv0.parent().map(Path::to_path_buf);
    }
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let mut names = vec![dir.join(argv0)];
        if cfg!(windows) && argv0.extension().is_none() {
            names.push(dir.join(argv0).with_extension("exe"));
        }
        if names
            .iter()
            .any(|c| c.canonicalize().is_ok_and(|c| c == me))
        {
            return Some(dir);
        }
    }
    None
}

/// Candidate config files, in priority order.
fn candidates(argv0: &OsStr) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut stems: Vec<String> = Vec::new();

    let argv0 = Path::new(argv0);
    if let Some(stem) = argv0.file_stem() {
        stems.push(stem.to_string_lossy().into_owned());
    }
    if let Some(dir) = invoked_dir(argv0) {
        dirs.push(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(stem) = exe.file_stem() {
            let stem = stem.to_string_lossy().into_owned();
            if !stems.contains(&stem) {
                stems.push(stem);
            }
        }
        if let Some(dir) = exe.parent() {
            if !dirs.iter().any(|d| d == dir) {
                dirs.push(dir.to_path_buf());
            }
        }
    }

    let mut out = Vec::new();
    for dir in &dirs {
        for stem in &stems {
            for ext in ["yaml", "yml"] {
                out.push(dir.join(format!("{stem}.{DEFAULT_CONFIG_NAME}.{ext}")));
            }
        }
    }
    for dir in &dirs {
        for ext in ["yaml", "yml"] {
            out.push(dir.join(format!("{DEFAULT_CONFIG_NAME}.{ext}")));
        }
    }
    out
}

pub fn load(argv0: &OsStr, rt: &Runtime) -> Result<Loaded, Failure> {
    let path = match &rt.config_path {
        Some(p) => {
            if !p.is_file() {
                return Err(cfg_err(format!(
                    "config file from {ENV_PREFIX}CONFIG not found: {}",
                    p.display()
                )));
            }
            Some(p.clone())
        }
        None => {
            let list = candidates(argv0);
            match list.iter().find(|p| p.is_file()) {
                Some(p) => Some(p.clone()),
                None if rt.target.is_some() => None,
                None => {
                    let searched: Vec<String> =
                        list.iter().map(|p| format!("  {}", p.display())).collect();
                    return Err(cfg_err(format!(
                        "no config file found; set {ENV_PREFIX}CONFIG or create one of:\n{}",
                        searched.join("\n")
                    )));
                }
            }
        }
    };

    let mut config = match &path {
        Some(p) => parse_file(p)?,
        None => Config::default(),
    };

    // Environment overrides win over the file.
    if let Some(t) = &rt.target {
        config.target = Some(t.clone());
    }
    if let Some(l) = &rt.log_file {
        config.log_file = Some(l.clone());
    }
    if let Some(b) = rt.preserve_argv0 {
        config.preserve_argv0 = b;
    }

    let base_dir = path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(Loaded {
        config,
        base_dir,
        path,
    })
}

pub fn parse_file(path: &Path) -> Result<Config, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| cfg_err(format!("cannot read {}: {e}", path.display())))?;
    parse_str(&text).map_err(|e| cfg_err(format!("{}: {e}", path.display())))
}

pub fn parse_str(text: &str) -> Result<Config, String> {
    let mut value: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    if value.is_null() {
        return Ok(Config::default());
    }
    interpolate(&mut value);
    serde_yaml_ng::from_value(value).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// ${env:VAR} / ${env:VAR:-default} interpolation
// ---------------------------------------------------------------------------

fn interpolate(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains("{env:") {
                *s = expand_env(s);
            }
        }
        Value::Sequence(seq) => seq.iter_mut().for_each(interpolate),
        Value::Mapping(map) => map.iter_mut().for_each(|(_, x)| interpolate(x)),
        Value::Tagged(t) => interpolate(&mut t.value),
        _ => {}
    }
}

fn expand_env(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\$?\$\{env:([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}").expect("valid regex")
    });
    re.replace_all(s, |c: &regex::Captures<'_>| {
        let whole = &c[0];
        if let Some(escaped) = whole.strip_prefix("$$") {
            return format!("${escaped}");
        }
        match std::env::var(&c[1]) {
            Ok(v) if !v.is_empty() => v,
            _ => c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let text = include_str!("../examples/argshim.yaml");
        let cfg = parse_str(text).expect("example config must parse");
        assert!(cfg.target.is_some());
        assert!(!cfg.rules.is_empty());
        crate::rules::compile(&cfg.rules).expect("example rules must compile");
    }

    #[test]
    fn env_interpolation() {
        std::env::set_var("ARGSHIM_TEST_INTERP", "hello");
        assert_eq!(expand_env("x-${env:ARGSHIM_TEST_INTERP}-y"), "x-hello-y");
        assert_eq!(expand_env("${env:ARGSHIM_TEST_MISSING:-dflt}"), "dflt");
        assert_eq!(expand_env("${env:ARGSHIM_TEST_MISSING}"), "");
        assert_eq!(
            expand_env("$${env:ARGSHIM_TEST_INTERP}"),
            "${env:ARGSHIM_TEST_INTERP}"
        );
    }

    #[test]
    fn position_variants() {
        let cfg = parse_str(
            r#"
target: x
rules:
  - { on: arg, action: add, value: a, position: start }
  - { on: arg, action: add, value: a, position: -1 }
  - { on: arg, action: add, value: a, position: { before: "--" } }
  - { on: arg, action: add, value: [a, 1, true], position: { after: run } }
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.rules[0].position,
            Some(PositionDef::Named(NamedPosition::Start))
        );
        assert_eq!(cfg.rules[1].position, Some(PositionDef::Index(-1)));
        assert_eq!(
            cfg.rules[2].position,
            Some(PositionDef::Before {
                before: "--".into()
            })
        );
        assert_eq!(
            scalar_list(cfg.rules[3].value.as_ref().unwrap()).unwrap(),
            vec!["a", "1", "true"]
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(
            parse_str("target: x\nrules:\n  - { on: env, action: remove, mtch: A }\n").is_err()
        );
    }
}

//! argshim — a transparent wrapper that rewrites arguments and environment
//! variables according to YAML rules and then runs the real executable.
//!
//! The wrapper never writes to stdout and only writes to stderr when it fails
//! itself or when `ARGSHIM_DEBUG` / `ARGSHIM_DRY_RUN` is set.

mod config;
mod exec;
mod pattern;
mod rules;

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;

use config::{Runtime, ENV_PREFIX, EXIT_CONFIG};
use rules::{EnvList, State};

/// A failure of the wrapper itself (as opposed to the target's exit code).
#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub msg: String,
}

/// Diagnostic sink: a log file, stderr (debug / dry-run) or nothing.
struct Logger {
    sink: Option<Box<dyn Write>>,
}

impl Logger {
    fn new(log_file: Option<&str>, to_stderr: bool) -> Self {
        let file = log_file.and_then(|p| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
                .map(|f| Box::new(f) as Box<dyn Write>)
        });
        let sink =
            file.or_else(|| to_stderr.then(|| Box::new(std::io::stderr()) as Box<dyn Write>));
        Self { sink }
    }

    fn enabled(&self) -> bool {
        self.sink.is_some()
    }

    fn line(&mut self, msg: impl AsRef<str>) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(w, "[argshim {}] {}", std::process::id(), msg.as_ref());
        }
    }
}

fn is_own_var(k: &OsString) -> bool {
    k.to_str().is_some_and(|k| {
        k.len() >= ENV_PREFIX.len()
            && k.is_char_boundary(ENV_PREFIX.len())
            && k[..ENV_PREFIX.len()].eq_ignore_ascii_case(ENV_PREFIX)
    })
}

fn log_env_diff(log: &mut Logger, before: &EnvList, after: &EnvList) {
    for (k, v) in before {
        match rules::env_get(after, k) {
            None => log.line(format!("env -{}", k.to_string_lossy())),
            Some(nv) if nv != v => log.line(format!(
                "env ~{}={:?}",
                k.to_string_lossy(),
                nv.to_string_lossy()
            )),
            _ => {}
        }
    }
    for (k, v) in after {
        if rules::env_get(before, k).is_none() {
            log.line(format!(
                "env +{}={:?}",
                k.to_string_lossy(),
                v.to_string_lossy()
            ));
        }
    }
}

fn run() -> Result<i32, Failure> {
    let mut raw = std::env::args_os();
    let argv0 = raw.next().unwrap_or_default();
    let args: Vec<OsString> = raw.collect();
    let original_env: EnvList = std::env::vars_os().collect();

    let rt = Runtime::from_env();
    let loaded = config::load(&argv0, &rt)?;
    let cfg = &loaded.config;

    // Dry run always reports to stderr; otherwise a log file wins over stderr.
    let log_file = if rt.dry_run {
        None
    } else {
        cfg.log_file.as_deref()
    };
    let mut log = Logger::new(log_file, rt.debug || rt.dry_run);
    if let Some(p) = &loaded.path {
        log.line(format!("config: {}", p.display()));
    }

    let compiled = rules::compile(&cfg.rules).map_err(|msg| Failure {
        code: EXIT_CONFIG,
        msg,
    })?;

    let mut state = State {
        args: args.clone(),
        env: original_env.clone(),
    };
    if rt.disable {
        log.line(format!("rules disabled via {ENV_PREFIX}DISABLE"));
    } else {
        for line in rules::apply(&compiled, &mut state, &rt.disabled_rules) {
            log.line(line);
        }
    }
    if cfg.strip_own_env {
        state.env.retain(|(k, _)| !is_own_var(k));
    }

    let target_name = cfg.target.as_deref().ok_or_else(|| Failure {
        code: EXIT_CONFIG,
        msg: format!("no target configured (set `target:` or {ENV_PREFIX}TARGET)"),
    })?;
    let target = exec::resolve(target_name, &loaded.base_dir, &state.env)?;

    if log.enabled() {
        log.line(format!("target: {}", target.display()));
        log.line(format!("args in:  {args:?}"));
        log.line(format!("args out: {:?}", state.args));
        log_env_diff(&mut log, &original_env, &state.env);
    }
    if rt.dry_run {
        log.line("dry run: target not started");
        return Ok(0);
    }
    drop(log);

    let argv0 = cfg.preserve_argv0.then_some(argv0.as_os_str());
    exec::run(&target, argv0, &state.args, &original_env, &state.env)
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(f) => {
            eprintln!("argshim: {}", f.msg);
            f.code
        }
    };
    std::process::exit(code);
}

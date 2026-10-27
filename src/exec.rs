//! Locating the real executable and handing control over to it.
//!
//! * Unix: the wrapper process is **replaced** via `execve(2)`, so PID,
//!   stdio, signals, controlling terminal and exit status belong to the target.
//! * Windows: there is no `exec`, so the target is spawned with inherited
//!   handles, the wrapper waits for it and exits with the same code.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::rules::{env_get, EnvList, ENV_CASE_INSENSITIVE};
use crate::Failure;

pub const EXIT_CANNOT_EXECUTE: i32 = 126;
pub const EXIT_NOT_FOUND: i32 = 127;

fn not_found(msg: String) -> Failure {
    Failure {
        code: EXIT_NOT_FOUND,
        msg,
    }
}

/// Possible file names for `path` (adds `PATHEXT` extensions on Windows).
fn candidates(path: &Path, env: &EnvList) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) && path.extension().is_none() {
        let pathext = env_get(env, OsStr::new("PATHEXT"))
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            let mut p = path.as_os_str().to_os_string();
            p.push(ext);
            out.push(PathBuf::from(p));
        }
    }
    out.push(path.to_path_buf());
    out
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Resolve `target` to an executable path.
///
/// * absolute path -> used as is;
/// * relative path containing a separator -> relative to `base_dir`
///   (the config file's directory);
/// * bare name -> searched in `PATH` of the *final* environment, skipping
///   the wrapper itself so a shim may share its name with the real binary.
pub fn resolve(target: &str, base_dir: &Path, env: &EnvList) -> Result<PathBuf, Failure> {
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let is_self = |p: &Path| match (&self_exe, p.canonicalize()) {
        (Some(s), Ok(c)) => *s == c,
        _ => false,
    };

    let path = Path::new(target);
    let has_separator = target.contains('/') || (cfg!(windows) && target.contains('\\'));

    if path.is_absolute() || has_separator {
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        };
        let found = candidates(&full, env)
            .into_iter()
            .find(|c| is_executable(c))
            .ok_or_else(|| {
                not_found(format!(
                    "target not found or not executable: {}",
                    full.display()
                ))
            })?;
        if is_self(&found) {
            return Err(Failure {
                code: EXIT_CANNOT_EXECUTE,
                msg: format!(
                    "target {} is the wrapper itself (infinite recursion)",
                    found.display()
                ),
            });
        }
        return Ok(found);
    }

    if let Some(path_var) = env_get(env, OsStr::new("PATH")) {
        for dir in std::env::split_paths(path_var) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            for c in candidates(&dir.join(target), env) {
                if is_executable(&c) && !is_self(&c) {
                    return Ok(c);
                }
            }
        }
    }
    Err(not_found(format!("target {target:?} not found in PATH")))
}

/// Build the command: arguments plus the *difference* between the original
/// and the final environment (so nothing invisible to `vars_os` is lost).
fn build(target: &Path, args: &[OsString], original_env: &EnvList, final_env: &EnvList) -> Command {
    let mut cmd = Command::new(target);
    cmd.args(args);
    for (k, _) in original_env {
        if env_get(final_env, k).is_none() {
            cmd.env_remove(k);
        }
    }
    for (k, v) in final_env {
        let unchanged = original_env.iter().any(|(ok, ov)| {
            ov == v
                && if ENV_CASE_INSENSITIVE {
                    ok.to_string_lossy()
                        .eq_ignore_ascii_case(&k.to_string_lossy())
                } else {
                    ok == k
                }
        });
        if !unchanged {
            cmd.env(k, v);
        }
    }
    cmd
}

fn spawn_error(target: &Path, e: std::io::Error) -> Failure {
    Failure {
        code: if e.kind() == std::io::ErrorKind::NotFound {
            EXIT_NOT_FOUND
        } else {
            EXIT_CANNOT_EXECUTE
        },
        msg: format!("cannot execute {}: {e}", target.display()),
    }
}

#[cfg(unix)]
pub fn run(
    target: &Path,
    argv0: Option<&OsStr>,
    args: &[OsString],
    original_env: &EnvList,
    final_env: &EnvList,
) -> Result<i32, Failure> {
    use std::os::unix::process::CommandExt;

    let mut cmd = build(target, args, original_env, final_env);
    if let Some(a0) = argv0 {
        cmd.arg0(a0);
    }
    // The Rust runtime ignores SIGPIPE; ignored dispositions survive execve,
    // so restore the default to keep the target's behaviour unchanged.
    // SAFETY: single-threaded at this point, plain libc call.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // `exec` only returns on failure.
    let err = cmd.exec();
    Err(spawn_error(target, err))
}

#[cfg(windows)]
pub fn run(
    target: &Path,
    _argv0: Option<&OsStr>,
    args: &[OsString],
    original_env: &EnvList,
    final_env: &EnvList,
) -> Result<i32, Failure> {
    use std::os::windows::io::AsRawHandle;

    let mut cmd = build(target, args, original_env, final_env);
    // Let Ctrl+C / Ctrl+Break reach the child (which shares the console) and
    // keep the wrapper alive until the child decides what to do.
    // A handler (unlike `SetConsoleCtrlHandler(NULL, TRUE)`) is not inherited.
    win::ignore_console_ctrl();

    let mut child = cmd.spawn().map_err(|e| spawn_error(target, e))?;
    let job = win::KillOnCloseJob::attach(child.as_raw_handle());
    let status = child.wait().map_err(|e| Failure {
        code: EXIT_CANNOT_EXECUTE,
        msg: format!("waiting for {} failed: {e}", target.display()),
    })?;
    // The child exited normally: let its own background processes survive.
    if let Some(job) = job {
        job.release();
    }
    // `code()` is always `Some` on Windows; the u32 exit code round-trips
    // through i32 -> `ExitProcess`.
    Ok(status.code().unwrap_or(1))
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe extern "system" fn swallow(_ctrl_type: u32) -> BOOL {
        1
    }

    pub fn ignore_console_ctrl() {
        // SAFETY: registering a static handler function.
        unsafe {
            SetConsoleCtrlHandler(Some(swallow), 1);
        }
    }

    /// Job object that kills the child tree if the wrapper itself is killed.
    pub struct KillOnCloseJob(HANDLE);

    fn set_limits(job: HANDLE, flags: u32) -> bool {
        // SAFETY: plain-old-data struct, valid job handle.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = flags;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
        }
    }

    impl KillOnCloseJob {
        pub fn attach(process: HANDLE) -> Option<Self> {
            // SAFETY: documented Win32 calls; handle closed on every error path.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
                if !set_limits(job, flags) || AssignProcessToJobObject(job, process) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

        /// Drop the kill-on-close flag; the handle stays open until exit.
        pub fn release(self) {
            set_limits(self.0, JOB_OBJECT_LIMIT_BREAKAWAY_OK);
            std::mem::forget(self);
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY: handle owned by this struct.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

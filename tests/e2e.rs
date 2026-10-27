//! End-to-end tests: run the compiled wrapper against real system tools.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn write_config(yaml: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("argshim-test-{}-{n}.yaml", std::process::id()));
    std::fs::write(&path, yaml).unwrap();
    path
}

fn wrapper(yaml: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_argshim"));
    cmd.env("ARGSHIM_CONFIG", write_config(yaml));
    cmd
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

#[test]
fn missing_config_is_reported() {
    let o = Command::new(env!("CARGO_BIN_EXE_argshim"))
        .env("ARGSHIM_CONFIG", "/definitely/not/here.yaml")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not found"));
}

#[test]
fn unknown_target_exits_127() {
    let o = wrapper("target: argshim-no-such-binary-xyz\n")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(127));
}

#[test]
fn target_pointing_to_wrapper_is_rejected() {
    let o = wrapper("target: whatever\n")
        .env("ARGSHIM_TARGET", env!("CARGO_BIN_EXE_argshim"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(126));
}

#[cfg(unix)]
mod unix {
    use super::*;

    #[test]
    fn exit_code_passthrough() {
        let o = wrapper("target: sh\n")
            .args(["-c", "exit 42"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(42));
    }

    #[test]
    fn signal_passthrough() {
        use std::os::unix::process::ExitStatusExt;
        let o = wrapper("target: sh\n")
            .args(["-c", "kill -TERM $$"])
            .output()
            .unwrap();
        assert_eq!(o.status.signal(), Some(15));
    }

    #[test]
    fn stdio_passthrough() {
        let mut child = wrapper("target: sh\n")
            .args(["-c", "cat; echo err >&2"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"hello\n").unwrap();
        let o = child.wait_with_output().unwrap();
        assert_eq!(stdout(&o), "hello\n");
        assert_eq!(String::from_utf8_lossy(&o.stderr), "err\n");
    }

    #[test]
    fn args_are_rewritten() {
        let cfg = r#"
target: sh
rules:
  - { on: arg, action: add, value: ['-c', 'printf "%s|" "$@"', sh], position: start }
  - { on: arg, action: remove, match: '--secret', with_value: true }
  - { on: arg, action: replace, match: '--level', scope: value, regex: '^debug$', replacement: info }
  - { on: arg, action: add, value: '--extra', position: { before: '--' } }
"#;
        let o = wrapper(cfg)
            .args(["a", "--secret", "xyz", "--level=debug", "--", "b"])
            .output()
            .unwrap();
        assert_eq!(stdout(&o), "a|--level=info|--extra|--|b|");
    }

    #[test]
    fn env_is_rewritten() {
        let cfg = r#"
target: env
rules:
  - { on: env, action: remove, match: 'REALM_NAME_*' }
  - { on: env, action: add, key: ARGSHIM_E2E_ADDED, value: yes }
  - { on: env, action: replace, match: 'ARGSHIM_E2E_OLD_*', rename: 'ARGSHIM_E2E_NEW_${1}' }
"#;
        let o = wrapper(cfg)
            .env("REALM_NAME_A", "1")
            .env("REALM_NAME_B", "2")
            .env("ARGSHIM_E2E_OLD_X", "v")
            .output()
            .unwrap();
        let out = stdout(&o);
        assert!(!out.contains("REALM_NAME_"), "{out}");
        // ARGSHIM_* (including ARGSHIM_CONFIG and rule-produced ones) is stripped by default
        assert!(!out.contains("ARGSHIM_"), "{out}");

        let cfg2 = format!("strip_own_env: false\n{cfg}");
        let o = wrapper(&cfg2)
            .env("ARGSHIM_E2E_OLD_X", "v")
            .output()
            .unwrap();
        let out = stdout(&o);
        assert!(out.contains("ARGSHIM_E2E_ADDED=yes"), "{out}");
        assert!(out.contains("ARGSHIM_E2E_NEW_X=v"), "{out}");
        assert!(!out.contains("ARGSHIM_E2E_OLD_X"), "{out}");
    }

    #[test]
    fn dry_run_does_not_start_target() {
        let o = wrapper("target: sh\n")
            .env("ARGSHIM_DRY_RUN", "1")
            .args(["-c", "exit 9"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&o.stderr).contains("dry run"));
    }

    #[test]
    fn disable_switch() {
        let cfg =
            "target: sh\nrules:\n  - { on: arg, action: replace, match: '-c', value: '-x' }\n";
        let o = wrapper(cfg)
            .env("ARGSHIM_DISABLE", "1")
            .args(["-c", "exit 3"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(3));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    #[test]
    fn exit_code_passthrough() {
        let o = wrapper("target: cmd\n")
            .args(["/C", "exit 42"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(42));
    }

    #[test]
    fn env_is_added() {
        let cfg = "target: cmd\nrules:\n  - { on: env, action: add, key: ARGSHIM_E2E_FOO, value: bar }\n  - { on: env, action: remove, match: 'REALM_NAME_*' }\nstrip_own_env: false\n";
        let o = wrapper(cfg)
            .env("REALM_NAME_A", "1")
            .args(["/C", "echo %ARGSHIM_E2E_FOO%-%REALM_NAME_A%"])
            .output()
            .unwrap();
        assert_eq!(stdout(&o).trim(), "bar-%REALM_NAME_A%");
    }

    #[test]
    fn args_are_added() {
        let cfg = "target: cmd\nrules:\n  - { on: arg, action: add, value: ['/C', 'exit 7'], position: start }\n";
        let o = wrapper(cfg).output().unwrap();
        assert_eq!(o.status.code(), Some(7));
    }
}

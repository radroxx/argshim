# argshim

[![CI](https://github.com/radroxx/argshim/actions/workflows/ci.yml/badge.svg)](https://github.com/radroxx/argshim/actions/workflows/ci.yml)
[![Release](https://github.com/radroxx/argshim/actions/workflows/release.yml/badge.svg)](https://github.com/radroxx/argshim/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**argshim** is a tiny, transparent process wrapper. It sits in front of a real
executable, rewrites its **command-line arguments** and **environment
variables** according to declarative YAML rules, and then hands control over
to the real program. Callers cannot tell the difference: stdin, stdout, stderr
and the exit code all belong to the target.

```text
caller ──► argshim ──(rewritten args + env)──► real binary
            ▲
            └── myapp.argshim.yaml
```

Typical uses:

- strip or rename environment variables a tool should not see (`REALM_NAME_*`, proxies, tokens);
- inject default flags, or force flags a user must not override;
- rename deprecated options after an upgrade (`--old-x` → `--new-x`);
- rewrite option values with regular expressions (`http://` → `https://`);
- per-OS or per-environment behaviour (CI, production) without touching the caller.

## Contents

- [Features](#features)
- [Installation](#installation)
- [Quick start](#quick-start)
- [How it works](#how-it-works)
- [Configuration](#configuration)
  - [Config file lookup](#config-file-lookup)
  - [Top-level settings](#top-level-settings)
  - [Environment overrides](#environment-overrides)
  - [Patterns](#patterns)
  - [Rules](#rules)
  - [Conditions](#conditions)
  - [Templates and captures](#templates-and-captures)
- [Recipes](#recipes)
- [Exit codes](#exit-codes)
- [Platform notes and limitations](#platform-notes-and-limitations)
- [Alternatives](#alternatives)
- [Building from source](#building-from-source)
- [License](#license)

## Features

- **Fully transparent.** On Unix the wrapper `exec`s the target — same PID,
  same stdio, signals and exit status. On Windows the target inherits all
  handles, receives Ctrl+C itself and its exit code is returned verbatim.
- **Silent.** The wrapper writes nothing to stdout, and to stderr only on its
  own failure or when debugging is explicitly enabled.
- **Declarative rules** for `env` and `arg` with `add`, `remove` and `replace` actions.
- **Glob or regex matching** — `REALM_NAME_*` or `re:^REALM_(\w+)$`.
- **Positional inserts** — `start`, `end`, an index (negative counts from the end),
  `before:` / `after:` another argument.
- **Regex replacement** with capture substitution (`$1`, `${name}`).
- **Conditions** per rule: OS, presence/absence/values of variables and arguments.
- **YAML config with environment overrides** (`ARGSHIM_*`) and `${env:VAR}` interpolation.
- Single static binary, no runtime dependencies. Linux and Windows (x86_64, aarch64).

## Installation

Download an archive for your platform from the
[Releases](https://github.com/your-name/argshim/releases) page, or build it
yourself:

```sh
cargo install --git https://github.com/your-name/argshim
```

## Quick start

Suppose you want every call to `terraform` to run without `REALM_NAME_*`
variables and always with `-no-color`.

**Linux / macOS** — put the wrapper earlier in `PATH` under the same name:

```sh
mkdir -p ~/.local/shims
cp argshim ~/.local/shims/argshim
ln -s argshim ~/.local/shims/terraform          # the shim takes the real name

cat > ~/.local/shims/terraform.argshim.yaml <<'EOF'
target: terraform            # bare name: found in PATH, the shim itself is skipped
rules:
  - { on: env, action: remove, match: "REALM_NAME_*" }
  - { on: arg, action: add, value: "-no-color", position: { after: "re:^(plan|apply)$" }, fallback: skip }
EOF

export PATH="$HOME/.local/shims:$PATH"
terraform plan               # runs: /usr/bin/terraform plan -no-color
```

**Windows** — copy the binary under the tool's name and place the config next to it:

```text
C:\shims\terraform.exe              (a copy of argshim.exe)
C:\shims\terraform.argshim.yaml     (target: C:\tools\terraform\terraform.exe)
```

Check what would be executed without running anything:

```sh
ARGSHIM_DRY_RUN=1 terraform plan
```

```text
[argshim 4242] config: /home/me/.local/shims/terraform.argshim.yaml
[argshim 4242] rule #1: applied
[argshim 4242] rule #2: applied
[argshim 4242] target: /usr/bin/terraform
[argshim 4242] args in:  ["plan"]
[argshim 4242] args out: ["plan", "-no-color"]
[argshim 4242] env -REALM_NAME_EU
[argshim 4242] dry run: target not started
```

A fully annotated configuration is in [`examples/argshim.yaml`](examples/argshim.yaml).

## How it works

1. Locate and parse the YAML config, interpolate `${env:VAR}`, apply `ARGSHIM_*` overrides.
2. Take the original arguments (without `argv[0]`) and environment.
3. Apply the rules **top to bottom**; each rule sees the result of the previous ones.
4. Remove `ARGSHIM_*` variables from the target's environment (unless `strip_own_env: false`).
5. Resolve `target` (using the *final* `PATH`) and run it.

Arguments and values that are not valid UTF-8 are never matched and are
passed through untouched.

## Configuration

### Config file lookup

The first existing file wins:

1. `$ARGSHIM_CONFIG`
2. `<dir>/<name>.argshim.yaml` (or `.yml`)
3. `<dir>/argshim.yaml` (or `.yml`)

`<name>` is the file stem the wrapper was invoked as (`argv[0]`) and of the
real executable; `<dir>` is the directory it was invoked from (symlinks are
**not** resolved, so one binary with many symlinks can have many configs) and
the directory of the real executable.

If no file is found but `ARGSHIM_TARGET` is set, the wrapper runs as a plain
pass-through.

### Top-level settings

| Key              | Default | Description |
|------------------|---------|-------------|
| `target`         | —       | Real executable. Absolute path; relative path with a separator (resolved against the config file's directory); or a bare name searched in `PATH` (with `PATHEXT` on Windows). Entries that are the wrapper itself are skipped. |
| `preserve_argv0` | `false` | Pass the wrapper's original `argv[0]` to the target (Unix only). Useful for multi-call binaries. |
| `strip_own_env`  | `true`  | Remove `ARGSHIM_*` variables before starting the target. |
| `log_file`       | —       | Append diagnostics to this file on every run. Does not touch stdout/stderr. |
| `rules`          | `[]`    | List of rules, applied in order. |

### Environment overrides

| Variable                 | Effect |
|--------------------------|--------|
| `ARGSHIM_CONFIG`         | Path to the config file. |
| `ARGSHIM_TARGET`         | Overrides `target`. |
| `ARGSHIM_LOG_FILE`       | Overrides `log_file`. |
| `ARGSHIM_PRESERVE_ARGV0` | Overrides `preserve_argv0` (`1/true/yes/on`). |
| `ARGSHIM_DISABLE`        | Skip all rules — pure pass-through. |
| `ARGSHIM_DISABLE_RULES`  | Comma-separated rule names to skip (unnamed rules are `#1`, `#2`, …). |
| `ARGSHIM_DEBUG`          | Print diagnostics to stderr (or to `log_file`). |
| `ARGSHIM_DRY_RUN`        | Print diagnostics to stderr and exit `0` without starting the target. |

Any string in the config may reference the wrapper's environment:

| Syntax                | Result |
|-----------------------|--------|
| `${env:VAR}`          | Value of `VAR`, empty if unset. |
| `${env:VAR:-default}` | Value of `VAR`, or `default` if unset or empty. |
| `$${env:VAR}`         | Literal `${env:VAR}`. |

Interpolation happens once, when the config is loaded, against the
**original** environment.

### Patterns

Used by `match`, `before`/`after` and conditions.

| Form                | Meaning |
|---------------------|---------|
| `REALM_NAME`        | Exact match. |
| `REALM_NAME_*`      | Glob: `*` = any sequence, `?` = one character. Anchored. Each wildcard is a capture group (`$1`, `$2`, …). |
| `re:^REALM_(\w+)$`  | Regular expression ([Rust `regex` syntax](https://docs.rs/regex/latest/regex/#syntax)). **Not** anchored automatically. |

On Windows, environment variable names are matched case-insensitively
(`path` matches `Path`). Use `(?-i)` inside a `re:` pattern to opt out.

**Arguments: key vs full.** By default (`match_on: key`) an argument of the
form `-x=value` / `--name=value` is matched by the part before `=`, so
`match: "--level"` hits both `--level debug` and `--level=debug`. With
`match_on: full` the whole argument is matched.

### Rules

Every rule has:

| Key       | Required | Description |
|-----------|----------|-------------|
| `on`      | yes      | `env` or `arg`. |
| `action`  | yes      | `add`, `remove` or `replace`. |
| `name`    | no       | Label for logs and `ARGSHIM_DISABLE_RULES`. |
| `enabled` | no       | `false` turns the rule off. |
| `when`    | no       | [Conditions](#conditions). |

Unknown keys and keys that make no sense for the chosen `on`/`action` are
rejected with an error, so typos never fail silently.

#### `env` / `add`

```yaml
- on: env
  action: add
  key: PATH                      # exact variable name
  value: ["/opt/a/bin", "/opt/b/bin"]   # scalar or list (list is joined with `separator`)
  if_exists: prepend             # overwrite (default) | skip | append | prepend
  separator: ":"                 # default ":" on Unix, ";" on Windows
```

#### `env` / `remove`

```yaml
- on: env
  action: remove
  match: "REALM_NAME_*"          # every matching variable is removed
```

#### `env` / `replace`

At least one of `value`, `regex` or `rename`:

```yaml
- on: env
  action: replace
  match: "REALM_NAME_*"
  rename: "APP_REALM_${1}"       # optional: new name, uses captures of `match`
  regex: "^dev-(.*)$"            # optional: rewrite the value...
  replacement: "prod-$1"         # ...with captures of `regex`
  # value: "fixed-${1}"          # or: replace the whole value with a template
```

#### `arg` / `add`

```yaml
- on: arg
  action: add
  value: ["--config", "/etc/app.toml"]   # one or several arguments, inserted contiguously
  position: start                # start | end (default) | <index> | {before: P} | {after: P}
  fallback: end                  # for before/after when P is absent: start | end (default) | skip
  match_on: key                  # how before/after patterns are matched
```

Indexes count user arguments (not `argv[0]`): `0` = first, `-1` = end,
`-2` = before the last one. Out-of-range indexes are clamped.

#### `arg` / `remove`

```yaml
- on: arg
  action: remove
  match: "--token"
  with_value: true               # also drop the following argument (`--token SECRET`);
                                 # `--token=SECRET` is a single argument anyway
```

#### `arg` / `replace`

Exactly one of `value` or `regex`:

```yaml
# Replace the whole argument with a template (captures from `match`):
- on: arg
  action: replace
  match: "--old-*"
  value: "--new-${1}"            # a list turns one argument into several

# Rewrite with a regex:
- on: arg
  action: replace
  match: "--url"
  scope: value                   # arg (default): the argument itself
                                 # value: text after '=' or the next argument
  regex: "^http://(.*)$"
  replacement: "https://$1"
```

### Conditions

All listed conditions must hold (logical AND). Each list may also be a single string.

```yaml
when:
  os: [linux, macos]             # any of; also `unix` / `windows` family
  env_set: ["CI"]                # each pattern matches some variable name
  env_unset: ["DEBUG*"]          # no variable name matches any of these
  env_match:                     # name pattern -> value pattern
    DEPLOY_ENV: "re:^prod"
  arg_present: ["--verbose"]     # each pattern matches some argument
  arg_absent: ["--config", "-c"] # no argument matches any of these
```

Argument conditions match either the key or the full argument. Conditions are
evaluated against the state **at that rule**, after earlier rules ran.

### Templates and captures

`value` (in `replace`), `rename` and `replacement` use
[regex replacement syntax](https://docs.rs/regex/latest/regex/struct.Captures.html#method.expand):
`$0` — whole match, `$1`/`${1}` — numbered group, `${name}` — named group,
`$$` — literal `$`.

Prefer the braced form when a group number is followed by text: `${1}_x`, not
`$1_x` (which means a group named `1_x`).

## Recipes

**Add a flag only if the user did not pass it:**

```yaml
- on: arg
  action: add
  value: "--color=never"
  when: { arg_absent: "--color" }
```

**Force a value regardless of what the user passed:**

```yaml
- { on: arg, action: remove, match: "--region", with_value: true }
- { on: arg, action: add, value: ["--region", "eu-west-1"], position: start }
```

**Insert options before positional arguments after `--`:**

```yaml
- on: arg
  action: add
  value: "--no-telemetry"
  position: { before: "--" }     # falls back to the end when there is no `--`
```

**Different real binary per machine:**

```yaml
target: ${env:MYTOOL_HOME:-/opt/mytool}/bin/mytool
```

## Exit codes

When the target runs, its exit code (or, on Unix, its termination signal) is
returned unchanged. The wrapper's own failures use codes borrowed from POSIX
shells and container runtimes:

| Code  | Meaning |
|-------|---------|
| `125` | Configuration error (missing/invalid file, invalid rule, no target). |
| `126` | Target found but cannot be executed (permissions, target is the wrapper itself). |
| `127` | Target not found. |

Errors are printed to stderr prefixed with `argshim:`.

## Platform notes and limitations

**Unix.** The wrapper replaces itself with the target via `execve(2)`, so
there is no extra process at all. `SIGPIPE` is reset to the default before
`exec` because the Rust runtime ignores it and ignored signals survive `exec`.

**Windows.** There is no `exec`. The target is started as a child with
inherited console and handles, the wrapper waits and exits with the child's
code. Specifically:

- Ctrl+C / Ctrl+Break are delivered to the target; the wrapper ignores them
  and waits until the target decides to exit.
- The target is placed in a job object, so killing the wrapper (e.g. from Task
  Manager) also terminates the target. After a normal exit the job releases
  its processes, so background processes started by the target survive.
- Windows has no argv: arguments are re-quoted with the standard
  `CommandLineToArgvW` rules. Programs that parse the raw command line in a
  non-standard way may see differences.
- argshim is a console-subsystem program; wrapping a GUI application will open
  a console window.
- `preserve_argv0` has no effect.

**General.**

- A target that is itself another argshim instance pointing back at the
  first one will loop forever; the wrapper only detects pointing at itself.
- Rules see only valid UTF-8 names/values; everything else passes unchanged.

## Alternatives

Before writing argshim we looked for an existing tool. Related projects solve
parts of the problem:

- **Shell scripts with `exec`** — flexible but per-tool, Unix-only, and easy to
  get quoting, signals and exit codes wrong.
- **Scoop / Chocolatey shims** (Windows) — redirect to a real executable and
  can prepend fixed arguments, but cannot remove or rewrite arguments or
  environment variables.
- **direnv**, **fastenv** and similar — manage environment per directory, but
  do not touch arguments and are not configured per executable.

None of them provide declarative add/remove/replace rules with glob/regex
matching for both arguments and environment.

## Building from source

Requires Rust 1.78+.

```sh
cargo build --release          # target/release/argshim
cargo test                     # unit + end-to-end tests
```

Windows release builds link the C runtime statically (see
`.cargo/config.toml`), so no Visual C++ redistributable is needed.

Releases are built by GitHub Actions for `x86_64`/`aarch64` Linux (static
musl and glibc) and `x86_64`/`aarch64` Windows when a `v*` tag is pushed:

```sh
git tag v0.1.0
git push origin v0.1.0
```

## License

[MIT](LICENSE)

# Slim relay · step 1 — the holder knows itself — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `rl-session` works out its own state (working / needs input / idle / stale, detail, last
line, foreground) from its own output and writes it to a `state.json` file, with nobody attached,
so the daemonless `rl` of step 2 can read a session's state without a daemon.

**Architecture:** The state vocabulary (`RunKind`, `RunState`, `Source`, `Confidence`, `Signal`)
moves from `rl-proto`/`rl-detect` down to `rl-core`; the PTY heuristics (`ansi.rs`, `pty.rs`) move
from `rl-detect` into `rl-session`. Old paths re-export the moved items, so the daemon and CLI
compile unchanged except one call site. The holder gains `--detect <FILE>` (patterns + timing as
JSON) and `--state-file <PATH>`; a small `Tracker` feeds the heuristics from the output pump and a
ticker and flushes `state.json` atomically. With neither flag the holder behaves exactly as today.

**Tech Stack:** Rust 2024 workspace, tokio (current-thread), serde/serde_json, regex, jiff.

**Spec:** `docs/specs/2026-11-07-slim-relay.md` (on `chore/roadmap`) — sections "Architecture: no
daemon", "Files", "How each moment works · Running", and "What exists and is reused · rl-session".

## Global Constraints

- One worktree + one PR off `main`, branch `slim-step-1`; `main` stays green.
- Before pushing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (a clippy warning is a CI error).
- No attribution lines in commits or the PR body.
- The holder↔daemon frame protocol does not change in this step (no new `Frame`, no tag changes); `rld` keeps running its own heuristics until step 5.
- `--no-tty` / `--no-input` stay this step: the old daemon still uses them for flows; they go with the daemon in step 5 (the roadmap line says step 1 — move it to step 5 when ticking).
- All file writes atomic: write `<path>.tmp`, then `rename` (as `exit.rs::write_exit_file`).
- Files a holder writes for the new `rl` live beside its socket: `~/.relay/sessions/<name>/{sock,state.json,exit}`; this step only needs the paths to be arguments.
- No backward compatibility shims beyond the re-exports named here.

## Review Focus

1. **Nobody attached for hours** — state must still advance (idle after quiet, stale after long quiet), because detection no longer waits for a daemon client. Test in Task 4 (`state_advances_with_nobody_attached`).
2. **`state.json` can't be written** (its folder deleted, read-only) — the holder must keep serving its child and its socket; only warn. Test in Task 4 (`unwritable_state_file_does_not_stop_the_holder`).
3. **A chatty child** (a spinner redrawing 60×/s) — writes must be bounded by the flush interval, not one per chunk. Test in Task 3 (`dirty_output_coalesces_into_one_flush`).
4. **A resting shell at its own prompt** must read idle, not needs-input — the foreground probe must run with nobody attached when detection is on. Test in Task 4 (`resting_shell_reads_idle_with_nobody_attached`).
5. **Child exits** — `state.json` keeps its last value and `exit` is written; a reader prefers `exit`. Test in Task 4 (`exit_leaves_state_and_exit_files`).

---

### Task 1: The state vocabulary lives in rl-core

**Files:**
- Create: `code/rl-core/src/state.rs`
- Modify: `code/rl-core/src/lib.rs` (add `pub mod state;`), `code/rl-core/Cargo.toml` (add `serde.workspace = true`)
- Modify: `code/rl-proto/src/types.rs:8-71` — delete the definitions of `RunKind`, `RunState` (with its `ALL`, `is_terminal`, `needs_attention`), `Source`, `Confidence`; replace with `pub use rl_core::state::{Confidence, RunKind, RunState, Source};`
- Modify: `code/rl-detect/src/machine.rs:19-55` — delete `Signal` and its impl; add `pub use rl_core::state::Signal;`
- Test: `code/rl-core/src/state.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `rl_core::state::{RunKind, RunState, Source, Confidence, Signal}` — identical names, derives, serde attributes (`rename_all = "snake_case"`), methods and `Signal::{pty, hook, manual}` constructors as today. `rl-proto/src/methods.rs:139-143` has an inherent `impl RunKind { fn default_command() }` used as a serde default: inherent impls can't stay on a foreign type, so replace it with a free `fn default_command_kind() -> RunKind` in `methods.rs` and point its `#[serde(default = …)]` at it.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn states_keep_their_wire_names() {
    assert_eq!(serde_json::to_string(&RunState::NeedsInput).unwrap(), "\"needs_input\"");
    assert_eq!(serde_json::to_string(&RunKind::Shell).unwrap(), "\"shell\"");
    let s = Signal::pty(RunState::Idle, Confidence::Likely, Some("$ ".into()));
    assert_eq!(s.source, Source::Pty);
    assert!(RunState::Failed.needs_attention() && RunState::Stopped.is_terminal());
}
```
(`serde_json` as a dev-dependency of rl-core.)

- [ ] **Step 2: Run it — `cargo test -p rl-core states_keep` — expect FAIL (module missing).**
- [ ] **Step 3: Move the types as listed; re-export from the old paths.**
- [ ] **Step 4: `cargo test --workspace` — expect PASS, nothing else changed.**
- [ ] **Step 5: Commit** `refactor: the run-state vocabulary lives in rl-core`

---

### Task 2: The PTY heuristics live in rl-session

**Files:**
- Create: `code/rl-session/src/detect/mod.rs` (`pub mod ansi; pub mod heuristics;`), move `code/rl-detect/src/ansi.rs` → `code/rl-session/src/detect/ansi.rs`, `code/rl-detect/src/pty.rs` → `code/rl-session/src/detect/heuristics.rs`
- Modify: `code/rl-session/src/lib.rs` (`pub mod detect;`), `code/rl-session/Cargo.toml` (add `regex.workspace = true`)
- Modify: `code/rl-detect/src/lib.rs` — replace `pub mod ansi; pub mod pty;` with `pub use rl_session::detect::{ansi, heuristics as pty};`
- Modify: `code/rl-detect/src/registry.rs:6` — `use rl_session::detect::heuristics::{Patterns, Timing};`
- Modify: `code/rl-daemon/src/supervisor.rs:770` — `Heuristics::new(profile.patterns.clone(), profile.timing, run.kind, Timestamp::now())`
- Test: tests in `pty.rs` that build a `Heuristics` from a `Registry` profile move to `code/rl-detect/src/profile_heuristics_tests.rs` (`#[cfg(test)] mod profile_heuristics_tests;` in `lib.rs`); tests needing only `Heuristics`/`ansi` move with their files.

**Interfaces:**
- Consumes: `rl_core::state::{RunKind, RunState, Confidence, Signal}` (Task 1).
- Produces: `rl_session::detect::heuristics::{Heuristics, Patterns, Timing, ShellCommand}` and `rl_session::detect::ansi::*`, unchanged except
  `Heuristics::new(patterns: Patterns, timing: Timing, kind: RunKind, now: Timestamp) -> Heuristics` (no `&Profile`).
  `ansi.rs`'s `use rl_session::screen::…` becomes `use crate::screen::…`.

- [ ] **Step 1: Move the files and change `new`'s signature; fix the imports above.**
- [ ] **Step 2: `cargo test --workspace` — expect PASS with the same test count as before the move (compare `cargo test --workspace 2>&1 | grep 'test result'` sums before/after).**
- [ ] **Step 3: Commit** `refactor: the PTY heuristics live in the holder's crate`

---

### Task 3: A detect spec and a tracker the holder can own

**Files:**
- Create: `code/rl-session/src/detect/spec.rs`, `code/rl-session/src/detect/tracker.rs`; add both to `detect/mod.rs`
- Create: `code/rl-session/src/state.rs` (beside `exit.rs`); add `pub mod state;` to `lib.rs`
- Test: `#[cfg(test)] mod tests` in each new file

**Interfaces:**
- Consumes: `Heuristics`, `Patterns`, `Timing` (Task 2); `RunKind`, `RunState`, `Confidence` (Task 1).
- Produces:
  ```rust
  // detect/spec.rs — what --detect <FILE> holds (JSON)
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct DetectSpec {
      pub kind: RunKind,
      pub needs_input: Vec<String>,   // regex sources
      pub idle: Vec<String>,
      pub idle_after_ms: u64,
      pub stale_after_ms: u64,
  }
  impl DetectSpec {
      pub fn from_parts(kind: RunKind, patterns: &Patterns, timing: Timing) -> Self; // Regex::as_str()
      pub fn compile(&self) -> anyhow::Result<(Patterns, Timing)>; // error names the bad pattern
  }

  // state.rs — what state.json holds
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct StateFile {
      pub state: RunState,
      pub confidence: Confidence,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub last_line: Option<String>,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub foreground: Option<bool>,
      pub at: Timestamp,
  }
  pub fn write_state_file(path: &Path, s: &StateFile) -> io::Result<()>;      // tmp + rename
  pub fn read_state_file(path: &Path) -> io::Result<Option<StateFile>>;       // Ok(None) if missing

  // detect/tracker.rs
  pub const LAST_LINE_MAX: usize = 200;
  pub struct Tracker { /* Heuristics + current StateFile + dirty */ }
  impl Tracker {
      pub fn new(patterns: Patterns, timing: Timing, kind: RunKind, now: Timestamp) -> Self; // starts Starting/Guess, dirty
      pub fn output(&mut self, raw: &[u8], now: Timestamp);
      pub fn tick(&mut self, now: Timestamp);
      pub fn foreground(&mut self, shell_owns_foreground: bool);
      pub fn take_dirty(&mut self) -> Option<StateFile>; // Some once per change, then None until the next
  }
  ```
  Rule (the only one carried over from `machine.rs::step`, since every signal here is PTY):
  `Stale` replaces only `Working` or `Starting`; every other signal sets state, confidence and
  detail. `last_line` is `Heuristics::last_line(LAST_LINE_MAX)` after each `output`. Any change to
  state, confidence, detail, last_line or foreground marks dirty.

- [ ] **Step 1: Write the failing tests**

```rust
// spec.rs
#[test]
fn a_spec_survives_json_and_compiles_back() {
    let spec = DetectSpec { kind: RunKind::Agent, needs_input: vec![r"\[y/N\]".into()],
        idle: vec![r"^❯\s*$".into()], idle_after_ms: 1500, stale_after_ms: 600_000 };
    let back: DetectSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
    let (p, t) = back.compile().unwrap();
    assert_eq!(p.needs_input[0].as_str(), r"\[y/N\]");
    assert_eq!(t.idle_after, Duration::from_millis(1500));
    assert_eq!(DetectSpec::from_parts(RunKind::Agent, &p, t), spec);
}
#[test]
fn a_bad_pattern_is_named() {
    let spec = DetectSpec { kind: RunKind::Agent, needs_input: vec!["(".into()], idle: vec![],
        idle_after_ms: 1, stale_after_ms: 1 };
    assert!(format!("{:#}", spec.compile().unwrap_err()).contains("("));
}

// tracker.rs (t(secs) helper as in the old pty tests)
#[test]
fn a_question_after_quiet_reads_needs_input() {
    let mut tr = tracker_with(r"\[y/N\]", 100, 10_000, RunKind::Agent);
    tr.output(b"Continue? [y/N] ", t(0.0));
    tr.tick(t(0.2));
    let s = tr.take_dirty().unwrap();
    assert_eq!(s.state, RunState::NeedsInput);
    assert_eq!(s.detail.as_deref(), Some("Continue? [y/N]"));
    assert_eq!(s.last_line.as_deref(), Some("Continue? [y/N]"));
    assert!(tr.take_dirty().is_none());
}
#[test]
fn stale_never_replaces_needs_input() {
    let mut tr = tracker_with(r"\[y/N\]", 100, 500, RunKind::Agent);
    tr.output(b"Continue? [y/N] ", t(0.0));
    tr.tick(t(0.2));
    tr.take_dirty();
    tr.tick(t(1.0));
    assert!(tr.take_dirty().is_none());
}
#[test]
fn dirty_output_coalesces_into_one_flush() {
    let mut tr = tracker_with(r"\[y/N\]", 100, 10_000, RunKind::Agent);
    for i in 0..100 { tr.output(format!("frame {i}\r").as_bytes(), t(0.0)); }
    assert!(tr.take_dirty().is_some());
    assert!(tr.take_dirty().is_none());
}

// state.rs
#[test]
fn a_state_file_round_trips_and_a_missing_one_is_none() { /* write to tempdir, read equal; read of absent path == Ok(None) */ }
```

- [ ] **Step 2: Run `cargo test -p rl-session --lib` — expect FAIL (types missing).**
- [ ] **Step 3: Implement the interfaces above.**
- [ ] **Step 4: Run `cargo test -p rl-session --lib` — expect PASS.**
- [ ] **Step 5: Commit** `feat(rl-session): a detect spec, a state file, and a tracker`

---

### Task 4: The holder runs its own tracker and writes state.json

**Files:**
- Modify: `code/rl-session/src/main.rs:19-75` (args), `:93-110` (Config)
- Modify: `code/rl-session/src/holder.rs` — `Config`, `Ctx`, `serve`, `pump_output`, `watch_foreground`; new task `watch_state`
- Modify: `code/rl-session/src/lib.rs:1-12` and `holder.rs:1-19` module docs: the holder now also knows its own state and needs no daemon to
- Test: `code/rl-session/tests/holder.rs` (reuse `start_argv`, `wait_gone`, `read_exit_file`)

**Interfaces:**
- Consumes: `DetectSpec::compile`, `Tracker`, `write_state_file`, `read_state_file` (Task 3).
- Produces (for step 2's `rl new`):
  - `rl-session --detect <FILE> --state-file <PATH> …` — `--detect` is a JSON `DetectSpec`; both optional; `--state-file` without `--detect` is an argument error; `--detect` without `--state-file` is allowed (tracker runs, nothing written).
  - `Config { …, detect: Option<(Patterns, Timing, RunKind)>, state_file: Option<PathBuf> }`.
  - `pub const STATE_FLUSH: Duration = Duration::from_millis(500);` and `pub const STATE_TICK: Duration = Duration::from_millis(250);` in `holder.rs`.
- Behaviour:
  - `pump_output` feeds each chunk to the tracker (`Ctx.tracker: Option<Mutex<Tracker>>`) under the existing lock order (shared first, then tracker) or right after dropping it.
  - `watch_state` ticks every `STATE_TICK`, calls `tracker.tick(now)`, and every `STATE_FLUSH` writes `take_dirty()` if `Some`; a write error logs `warn!` once per distinct error kind and the holder carries on.
  - `watch_foreground`: when a tracker exists, it samples even with no client attached (the `while clients == 0` wait applies only without a tracker); each published change also calls `tracker.foreground(v)`. The deep-idle park on `output_wake` stays.
  - After the child exits, `watch_state` flushes once more and stops; `state.json` is left as is.

- [ ] **Step 1: Write the failing integration tests** (helper `start_detecting(program_args, spec_json) -> (Holder, PathBuf /*state*/)` writes the spec to the tempdir and passes `--detect` / `--state-file`; helper `async fn wait_state(path, pred) -> StateFile` polls `read_state_file` every 50 ms up to 10 s)

```rust
#[tokio::test]
async fn state_advances_with_nobody_attached() {
    // sh -c 'printf "Continue? [y/N] "; sleep 30', needs_input [y/N], idle_after 200ms
    let s = wait_state(&state, |s| s.state == RunState::NeedsInput).await;
    assert_eq!(s.detail.as_deref(), Some("Continue? [y/N]"));
}
#[tokio::test]
async fn a_quiet_worker_goes_stale_with_nobody_attached() {
    // kind command, sh -c 'echo working; sleep 30', stale_after 500ms
    wait_state(&state, |s| s.state == RunState::Stale).await;
}
#[tokio::test]
async fn resting_shell_reads_idle_with_nobody_attached() {
    // kind shell, program /bin/sh -i, needs_input pattern r"\$\s*$" (would match the PS1), idle_after 200ms
    let s = wait_state(&state, |s| s.state == RunState::Idle).await;
    assert_eq!(s.foreground, Some(true));
}
#[tokio::test]
async fn exit_leaves_state_and_exit_files() {
    // sh -c 'echo bye; exit 3'
    // wait for exit file: code 3; state.json still readable and its last_line is "bye"
}
#[tokio::test]
async fn unwritable_state_file_does_not_stop_the_holder() {
    // --state-file <tmp>/missing-dir/state.json; holder becomes ready, attach works, read_until("fake-cli ready")
}
#[tokio::test]
async fn without_detect_no_state_file_appears() {
    // existing start(true, 5); after 1 s no state.json next to the socket; pty_lifecycle behaviour unchanged
}
```

- [ ] **Step 2: Run `cargo test -p rl-session --test holder` — expect the six new tests to FAIL (unknown argument `--detect`).**
- [ ] **Step 3: Add the args, `Config` fields, `Ctx.tracker`, the `pump_output` feed, `watch_state`, and the `watch_foreground` change as specified.**
- [ ] **Step 4: Run `cargo test --workspace` — expect PASS, including every existing holder test.**
- [ ] **Step 5: Run the CI gates** (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`) — expect clean.
- [ ] **Step 6: Commit** `feat(rl-session): the holder knows its own state and writes state.json`

---

## After the PR merges

- On `chore/roadmap`: tick step 1 with the PR number, and move "`--no-tty` goes" from step 1 to step 5.
- Kill any running `rld`/`rl` per the no-backward-compat rule (the holder binary changed).

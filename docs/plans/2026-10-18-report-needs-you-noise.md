# Report: quiet the needs-you band Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the report's `needs you` band from filling with noise — resting shells (finding #3) and stale weeks-old runs (finding #2).

**Architecture:** Two independent fixes landing in one branch/PR.
- **#2 (aging):** `project_builtins` gains `now` and drops a built-in *run* lead that would sit in `needs you` once its `state_at` is older than a cutoff. Pure, report-layer, fully unit-tested.
- **#3 (resting shells):** a foreground-process-group signal. The PTY master fd lives in the holder process (`rl-session`); the classifier (`rl-detect::Heuristics`) runs in the daemon and only sees output bytes over a Unix socket. So the holder computes `tcgetpgrp(master) == child_pid` ("the shell itself owns the terminal"), ships it as a new `Frame::Foreground(bool)` on change, and the daemon feeds it into the classifier, which forces `Idle` (never `NeedsInput`) for a shell whose own prompt owns the foreground. A genuinely-blocked child (sudo, `read -p`) owns the foreground instead, so it still surfaces.

**Tech Stack:** Rust workspace. `nix` 0.31 (already a dep, `term` feature on) for `tcgetpgrp`. `portable-pty` 0.9 master (`as_raw_fd`). Tokio.

**Spec:** `docs/specs/2026-10-17-the-report.md` (the report arc; on `chore/roadmap`). These are step-1 dogfood follow-ups folded forward; no separate spec doc.

## Global Constraints

- **No backward compat.** The holder↔daemon frame protocol (`rl-session/src/proto.rs`) may gain a frame; all `rld`/`rl`/holders are killed and rebuilt on upgrade. No fallback for peers that predate the frame.
- **CI gates (must pass before push):** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (a warning is a CI error), `cargo test --workspace`. CI runs on `ubuntu-latest` AND `macos-latest`; `tcgetpgrp` is POSIX and compiles on both.
- `Timestamp` everywhere is `jiff::Timestamp` (no re-export).
- Built-in lead source identity is `relay` (`BUILTIN_SOURCE`), keys `decision:/run:/firing:<id>`.
- Idle cost of the foreground poll must stay near zero: poll only while a client is attached, back off / park on sustained quiet, and emit a frame only on change.

## Review Focus

- **A non-shell PTY run must be unaffected by the foreground signal.** A dev server that goes quiet must NOT be forced to `Idle` by fg ownership — the force-Idle applies only when the run is a shell. (Test in Task 2.)
- **Unknown foreground (pipe run, or before the first frame) preserves today's behavior.** `set_foreground` never called, or a pipe backend that reports no fg → classifier behaves exactly as it does today. (Test in Task 2.)
- **A recent failure still reaches `needs you`.** The aging drop must fire only past the cutoff; a run that failed a minute ago stays. Boundary test at the cutoff. (Test in Task 1.)
- **Aging only drops run leads.** An open decision (needs you forever) and a flow firing must never be dropped by the age cutoff, however old. (Test in Task 1.)
- **The foreground watcher must not spin when idle and must stop when no client is attached.** Park on an output notify once quiet; gate on `clients > 0`. (Design-verified in Task 5; integration/manual.)

---

### Task 1: Age stale run leads out of needs-you (#2)

**Files:**
- Modify: `code/rl-daemon/src/report.rs` (`project_builtins` signature + run-lead loop; new `STALE_NEEDS_YOU` const)
- Modify: `code/rl-daemon/src/handlers.rs` (the one `project_builtins` call site, ~line 433)
- Test: `code/rl-daemon/src/report.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `Run.state_at: jiff::Timestamp` (when state last changed), `RunState::needs_attention()`.
- Produces: `project_builtins(decisions: &[Decision], runs: &[Run], firings: &[Firing], now: jiff::Timestamp) -> Vec<Lead>` (added `now`).

- [ ] **Step 1: Write the failing tests**

```rust
// in report.rs tests
#[test]
fn stale_needs_you_run_is_dropped() {
    let now = ts("2026-10-18T00:00:00Z");
    let old = run_in_state("r1", RunState::Failed, now - Duration::from_secs(8 * 24 * 3600));
    let leads = project_builtins(&[], &[old], &[], now);
    assert!(leads.is_empty(), "a run failed 8 days ago is stale, not news");
}

#[test]
fn recent_needs_you_run_is_kept() {
    let now = ts("2026-10-18T00:00:00Z");
    let fresh = run_in_state("r1", RunState::Failed, now - Duration::from_secs(60));
    let leads = project_builtins(&[], &[fresh], &[], now);
    assert_eq!(leads.len(), 1);
    assert_eq!(leads[0].band, Band::NeedsYou);
}

#[test]
fn aging_never_drops_decisions_or_firings() {
    let now = ts("2026-10-18T00:00:00Z");
    // an open decision created long ago still needs you
    let d = open_decision("d1", now - Duration::from_secs(30 * 24 * 3600));
    let leads = project_builtins(&[d], &[], &[], now);
    assert_eq!(leads.len(), 1, "an old open decision is still needs-you");
}
```

- [ ] **Step 2: Run them, watch them fail** (`project_builtins` arity is wrong → compile error first; fix test helpers/signature mentally, then assertion failure). Run: `cargo test -p rl-daemon report::` — expect FAIL.

- [ ] **Step 3: Implement.** Add the const and the drop. A stale run lead is dropped only when it would land in `needs you`:

```rust
/// How long a run may sit needing a human before it stops being news. Past
/// this, a Failed/Blocked/NeedsInput run drops out of the report entirely
/// rather than piling up in needs-you forever. Tunable.
const STALE_NEEDS_YOU: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

pub fn project_builtins(
    decisions: &[Decision],
    runs: &[Run],
    firings: &[Firing],
    now: jiff::Timestamp,
) -> Vec<Lead> {
    // ... decisions loop unchanged ...
    for r in runs {
        if r.is_shell() && !r.state.needs_attention() && !r.state.is_terminal() {
            continue;
        }
        let band = if r.state.needs_attention() {
            Band::NeedsYou
        } else if r.state.is_terminal() {
            Band::Happened
        } else {
            Band::Live
        };
        // A run that has needed a human longer than STALE_NEEDS_YOU is no
        // longer news — drop it rather than let it sit in needs-you forever.
        if band == Band::NeedsYou && (now - r.state_at).unsigned_abs() > STALE_NEEDS_YOU {
            continue;
        }
        // ... push lead unchanged ...
    }
    // ... firings loop unchanged ...
}
```
(Note `jiff::Timestamp - Timestamp = jiff::Span`; compare via `.unsigned_abs()` against a `std::time::Duration` using `Span`→`Duration` or compute with `now.duration_since(r.state_at)`. Use whichever jiff API the crate already uses elsewhere — `digest.rs`/`summaries.rs` have the precedent; match it.)

- [ ] **Step 4: Update the call site** in `handlers.rs`: `project_builtins(&decisions, &runs, &firings, now)` (the handler already binds `now`).

- [ ] **Step 5: Run `cargo test -p rl-daemon`** — expect PASS, and the existing `run_state_maps_to_band` / `since_bounds_only_happened` still green (they use recent timestamps; confirm, adjust their `now`/`at` if any used a far-past date).

- [ ] **Step 6: Commit** — `fix(report): age stale runs out of needs-you`.

---

### Task 2: Foreground-aware shell classification (#3, pure core)

**Files:**
- Modify: `code/rl-detect/src/pty.rs` (`Heuristics`: field + `set_foreground`, consulted in `classify`)
- Test: `code/rl-detect/src/pty.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `Heuristics::set_foreground(&mut self, shell_owns_foreground: bool)` — records the latest foreground ownership. `None` (never set) = unknown = today's behavior.
- The heuristic already holds the run kind (`Heuristics::new(profile, kind, now)`); `kind == RunKind::Shell` (or the profile's shell flag) gates the force.

- [ ] **Step 1: Write the failing tests** (pure, no I/O):

```rust
#[test]
fn shell_owning_foreground_is_idle_not_needs_input() {
    let mut h = Heuristics::new(shell_profile(), RunKind::Shell, t0());
    h.set_foreground(true); // the shell itself is at its prompt
    // feed a prompt line the needs_input regex would otherwise catch, then go quiet
    h.output(b"user@host ~ $ ", t0());
    let sig = h.tick(t0() + idle_after()); // classify fires
    assert_eq!(sig.unwrap().state, RunState::Idle);
}

#[test]
fn shell_with_child_in_foreground_can_still_need_input() {
    let mut h = Heuristics::new(shell_profile(), RunKind::Shell, t0());
    h.set_foreground(false); // a child program owns the terminal
    h.output(b"Password: ", t0());
    let sig = h.tick(t0() + idle_after());
    assert_eq!(sig.unwrap().state, RunState::NeedsInput);
}

#[test]
fn non_shell_run_ignores_foreground() {
    let mut h = Heuristics::new(server_profile(), RunKind::Command, t0());
    h.set_foreground(true);
    h.output(b"Continue? [y/N] ", t0());
    let sig = h.tick(t0() + idle_after());
    assert_eq!(sig.unwrap().state, RunState::NeedsInput); // unchanged behavior
}

#[test]
fn unknown_foreground_preserves_todays_behavior() {
    let mut h = Heuristics::new(shell_profile(), RunKind::Shell, t0());
    // set_foreground never called
    h.output(b"Continue? [y/N] ", t0());
    let sig = h.tick(t0() + idle_after());
    assert_eq!(sig.unwrap().state, RunState::NeedsInput);
}
```
(Match existing test helper names/patterns in `pty.rs`; the four cases are the contract.)

- [ ] **Step 2: Run, watch fail** — `cargo test -p rl-detect pty::` — `set_foreground` missing → compile fail, then assertion fails.

- [ ] **Step 3: Implement.** Add `shell_fg: Option<bool>` to `Heuristics`, a setter, and gate in `classify`:

```rust
pub fn set_foreground(&mut self, shell_owns_foreground: bool) {
    self.shell_fg = Some(shell_owns_foreground);
}
```
In `classify`, before the `needs_input` regex branch: if the run is a shell and `self.shell_fg == Some(true)`, return the `Idle` signal (same confidence path the idle branch uses) and never consult `needs_input`. Leave every other path (child in foreground, non-shell, unknown) exactly as is.

- [ ] **Step 4: Run `cargo test -p rl-detect`** — expect PASS, all existing pty tests green.

- [ ] **Step 5: Commit** — `feat(detect): a shell owning the terminal reads idle, not needs-input`.

---

### Task 3: `Frame::Foreground` on the holder wire

**Files:**
- Modify: `code/rl-session/src/proto.rs` (variant, tag `0x86`, encode/decode, round-trip test)
- Test: `code/rl-session/src/proto.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `Frame::Foreground(bool)` — holder → client. Payload is one byte (`0`/`1`), fixed width.

- [ ] **Step 1: Failing round-trip test**

```rust
#[test]
fn foreground_frame_round_trips() {
    for b in [true, false] {
        let f = Frame::Foreground(b);
        let bytes = f.encode();
        let got = Frame::decode(bytes[0], bytes[5..].to_vec()).unwrap();
        assert_eq!(got, Frame::Foreground(b));
    }
}
```

- [ ] **Step 2: Run, watch fail** — `cargo test -p rl-session proto::` — variant missing → compile fail.

- [ ] **Step 3: Implement.** Add `const T_FOREGROUND: u8 = 0x86;`, the variant `Foreground(bool)` (holder → client section), encode arm `Frame::Foreground(b) => (T_FOREGROUND, vec![*b as u8])`, decode arm:
```rust
T_FOREGROUND => {
    if p.len() != 1 { return Err(bad("foreground")); }
    Frame::Foreground(p[0] != 0)
}
```

- [ ] **Step 4: Fix the exhaustive matches the new variant breaks.** The compiler enumerates them (`client.rs`, `held.rs`, anywhere matching `Frame`). For now every consumer except the daemon ignores it (`Frame::Foreground(_) => {}` / passes through). Wire the daemon's use in Task 6.

- [ ] **Step 5: Run `cargo test -p rl-session`** — PASS.

- [ ] **Step 6: Commit** — `feat(session): Frame::Foreground carries terminal foreground ownership`.

---

### Task 4: Foreground probe in the backend

**Files:**
- Modify: `code/rl-session/src/backend.rs` (`Spawned.foreground`, probe built in `spawn_pty`; `None` in `spawn_pipes`)

**Interfaces:**
- Produces: `pub type Foreground = Arc<dyn Fn() -> Option<bool> + Send + Sync>;` and `Spawned.foreground: Option<Foreground>`. The closure returns `Some(true)` when the shell (pid == its own process-group leader, guaranteed by the pty's setsid) owns the terminal foreground, `Some(false)` when a child does, `None` when it can't be read.

- [ ] **Step 1: Implement the probe** in `spawn_pty`, holding an `Arc` clone of the master so the fd stays valid for the probe's life:

```rust
let fg_master = master.clone();
let foreground: Foreground = Arc::new(move || {
    let m = fg_master.lock().ok()?;
    let fd = m.as_raw_fd()?; // MasterPty::as_raw_fd -> Option<RawFd>
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    let fg = nix::unistd::tcgetpgrp(borrowed).ok()?;
    Some(fg.as_raw() == pid as i32) // child pid == its pgid (pty setsid'd)
});
// Spawned { ..., foreground: Some(foreground) }
```
`spawn_pipes` sets `foreground: None`.

- [ ] **Step 2: Build** — `cargo build -p rl-session`. Resolve the exact `nix::unistd::tcgetpgrp` signature against nix 0.31 (it takes `AsFd`, returns `Pid`); adjust the borrow if the compiler wants `&BorrowedFd`/owned. No unit test here — the probe needs a live pty; it is exercised by Task 5's wiring and manual verification.

- [ ] **Step 3: Commit** — `feat(session): backend exposes a terminal-foreground probe`.

---

### Task 5: Holder foreground watcher + per-client frame

**Files:**
- Modify: `code/rl-session/src/holder.rs` (`Ctx.fg` watch, `watch_foreground` task, output notify, `handle_client` emits the frame)

**Interfaces:**
- Consumes: `Spawned.foreground` (Task 4), `Frame::Foreground` (Task 3), `ctx.clients` (already tracked).
- Produces: holder → client `Frame::Foreground(bool)` on change and once on attach.

- [ ] **Step 1: Add the channel + notify to `Ctx`:** `fg: watch::Sender<bool>` (initial `true` — a fresh shell rests at its prompt) and `output_wake: Arc<tokio::sync::Notify>`. Bump `output_wake` in `pump_output` on each chunk.

- [ ] **Step 2: Spawn `watch_foreground(ctx, probe)`** when `spawned.foreground.is_some()`:
  - Gate: if `ctx.clients == 0`, wait on a client (short sleep or a clients notify) — nobody is classifying, so don't poll.
  - Active: `interval(1s)`; each tick call the probe; `ctx.fg.send_if_modified` so a frame goes out only on change.
  - Quiet-window park: after ~8 consecutive ticks with no change AND no output since, stop ticking and `output_wake.notified().await` — a resting shell then costs zero syscalls/wakeups until the next output; resume 1s polling when it fires.

- [ ] **Step 3: `handle_client`** subscribes `ctx.fg.subscribe()`, writes `Frame::Foreground(*rx.borrow())` once after `Hello`/replay, then in its select loop writes it again whenever `rx.changed()` fires. Must share the connection's `select!` via the same cancel-safe path as output.

- [ ] **Step 4: Build + targeted manual check.** `cargo build -p rl-session`. Manual (recorded in the PR): start a shell run, `rl report` at its prompt → not in needs-you; run `sleep 30` in it → still not needs-you (shell handed foreground to a child that isn't prompting → live); run something that blocks on input → needs-you.

- [ ] **Step 5: Commit** — `feat(session): holder polls terminal foreground and frames it to clients`.

---

### Task 6: Daemon feeds foreground into the classifier

**Files:**
- Modify: `code/rl-daemon/src/supervisor.rs` (`attached` loop handles `Frame::Foreground`)

**Interfaces:**
- Consumes: `Frame::Foreground(bool)` (Task 3), `Heuristics::set_foreground` (Task 2).

- [ ] **Step 1: Handle the frame** in the `attached` read loop, beside the `Frame::Output` arm:
```rust
Ok(Some(Frame::Foreground(shell_fg))) => {
    heur.set_foreground(shell_fg);
    // optionally re-evaluate immediately so a transition is reflected
    // without waiting a full tick:
    if let Some(sig) = heur.tick(Timestamp::now()) {
        self.consider(&live, sig).await;
    }
}
```
(If an immediate re-tick risks a spurious transition, just record it and let the next scheduled tick pick it up — the 250ms `DETECT_TICK` is fast enough. Choose the simpler of the two; note the choice in the report.)

- [ ] **Step 2: Build the workspace** — `cargo build --workspace`.

- [ ] **Step 3: Full gate** — `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. All green.

- [ ] **Step 4: End-to-end manual verification** (record results in the PR): the Task 5 manual sequence, now with the daemon actually classifying — confirm `rl report` keeps resting shells out of needs-you while a genuinely blocked shell still appears, and that an idle machine shows no measurable CPU from the polling.

- [ ] **Step 5: Commit** — `feat(daemon): foreground ownership drives shell idle/needs-input`.

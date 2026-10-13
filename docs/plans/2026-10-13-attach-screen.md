# Attach Draws the Screen, Not a Byte Log — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Watching or joining a terminal run draws the run's current screen (and the history above it) once, drawn afresh, instead of replaying a tail of raw bytes, so inline apps like Gemini CLI and Claude Code no longer show repeated or garbled content when you reattach, and the repaint nudge goes away.

**Architecture:** The session holder keeps a headless `vt100` terminal per tty run. It is fed every output byte under the same lock as the ring and resized with the pty. A viewer's attach asks the holder for a *screen* replay. The holder answers with one `Output` frame that redraws the emulator's history and then the visible screen (`Screen::snapshot`), then `ReplayEnd`, then live bytes. For an ended tty run, `rld` builds the same snapshot from the journal tail. Pipes runs keep the byte replay, since their output is lines, not a screen. Because what a viewer is handed is already the screen, nothing needs to make the app repaint, so the post-replay nudge, the watcher nudge, the "leave it a row short" on letting go and the shared nudge state in `Live` all go. One small nudge stays: a run joined younger than `YOUNG` gets one more resize after a delay, because its first resize may arrive before the app listens for it. That concerns the pty size, not the replay.

**Tech Stack:** Rust workspace; `vt100 = "0.16"` (already a workspace dependency); tokio; the holder's frozen framed protocol (`rl-session/src/proto.rs`).

**Spec:** Agreed in conversation (2026-10-13): "Put a headless terminal emulator between the app and the viewers, and treat its screen as the truth: holder owns a `vt100::Parser`, attach sends a snapshot not a byte tail, the viewer draws on a clean surface, drop the nudge in most cases." The user then asked to "drop all dead code".

## Global Constraints

- `rl-session` stays tiny and frozen-protocol: the only new dependency allowed in it is `vt100` (workspace). It must not depend on any other Relay crate except `rl-core`, as now.
- Old holders keep working with a new `rld` (holders outlive daemon upgrades): a holder that does not answer with a screen gets exactly today's byte path (journal tail + replay from `journaled`), without any nudge.
- The holder's `Attach` frame stays compatible both ways: a 1-byte payload and the existing 9-byte payload still decode; an old holder reading the new screen request must see "replay from `from`".
- History kept by the holder's emulator: `HISTORY_ROWS = 2000` (bounded memory: 32 bytes/cell).
- No dead code left behind: every item only the nudge or the old raw tty replay used is deleted, along with its tests and docs.
- Commits: no attribution lines. Run `cargo test --workspace` (integration tests need the `rl-session` binary built by the workspace), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
- Comment style: match the codebase. Doc comments say *why*, in plain sentences, in the voice of the surrounding code.

## Review Focus

1. **A run whose output has scrolled a lot (Gemini's conversation).** Reattaching shows the history once, in order, above the live screen. Pinned by `snapshot_round_trips_history_and_screen` (Task 1).
2. **An app that redraws one line in place hundreds of times** (spinners, Ink's erase-and-rewrite). The reattach shows only the last frame. Pinned by `a_line_redrawn_in_place_is_drawn_once` (Task 1) and `a_watcher_sees_the_screen_not_every_redraw` (Task 3).
3. **A full-screen app on the alternate screen.** The snapshot switches the viewer to the alternate screen before drawing, so the app's later `?1049l` returns the viewer cleanly. Pinned by `an_alternate_screen_app_is_drawn_on_the_alternate_screen` (Task 1).
4. **The box-drawing set (`ESC ( 0`).** The snapshot carries `─`, not `q`. Pinned by `the_box_drawing_set_survives_the_snapshot` (Task 1).
5. **A joiner that comes and goes at the run's own size.** The run hears no resize at all, neither when it joins nor when it lets go. Pinned by `joining_and_letting_go_at_the_runs_size_is_no_resize` (Task 3).

---

### Task 1: `rl_session::screen` — the headless terminal and its snapshot

Moves the box-drawing rewrite (`Graphics`, `dec_graphic`) from `rl-detect` into `rl-session`, because the holder needs it and `rl-session` must not depend on `rl-detect`. Adds `Screen`.

**Files:**
- Create: `code/rl-session/src/screen.rs`
- Modify: `code/rl-session/src/lib.rs` (add `pub mod screen;`, mention it in the crate doc's library half)
- Modify: `code/rl-session/Cargo.toml` (add `vt100.workspace = true`)
- Modify: `code/rl-detect/Cargo.toml` (add `rl-session.workspace = true`; check the workspace `[workspace.dependencies]` has `rl-session = { path = ... }`, and add it if not)
- Modify: `code/rl-detect/src/ansi.rs`: delete `Graphics`, its `impl`, and its tests. Make `Stripper` use `rl_session::screen::dec_graphic` and delete the local `dec_graphic`. Keep `EscState`/`overrun` only if `Stripper` still uses them (it does).
- Modify: `code/rl-cli/src/tui/term.rs`: `rl_detect::ansi::Graphics` → `rl_session::screen::Graphics`.

**Interfaces:**
- Produces:
  - `pub fn dec_graphic(c: char) -> char` (moved verbatim)
  - `pub struct Graphics` with `pub fn new() -> Self` and `pub fn rewrite(&mut self, bytes: &[u8]) -> Vec<u8>` (moved verbatim, with whatever private escape-state items it needs copied into `screen.rs` and trimmed to what `Graphics` itself uses)
  - `pub const HISTORY_ROWS: usize = 2000;`
  - `pub struct Screen` with:
    - `pub fn new(cols: u16, rows: u16) -> Self` (zero sizes clamp to 1)
    - `pub fn feed(&mut self, bytes: &[u8])` (runs `Graphics::rewrite` first, then `vt100::Parser::process`)
    - `pub fn resize(&mut self, cols: u16, rows: u16)`
    - `pub fn snapshot(&mut self) -> Vec<u8>`
    - `pub fn render(bytes: &[u8], cols: u16, rows: u16) -> Vec<u8>`: an associated convenience, `new` + `feed` + `snapshot`, for `rld`'s ended runs.

- [ ] **Step 1: Move `Graphics` and `dec_graphic`, keep everything green**

Copy `dec_graphic` and `Graphics` (with the parts of `EscState`/`overrun` that `Graphics::rewrite` needs, as private items) into `code/rl-session/src/screen.rs` under a module doc:

```rust
//! A headless terminal: the run's screen as a viewer would see it.
//!
//! Replaying a tail of output bytes to someone attaching is not handing them
//! a screen. The tail starts in the middle of a redraw, it carries every
//! redraw the app ever made, and an inline app (Gemini CLI, Claude Code) that
//! reprints its history on a resize leaves each reprint in it. Played into a
//! terminal of another size, relative cursor moves land on the wrong lines.
//! So the holder keeps the screen itself, fed every byte, and an attach is
//! handed that screen drawn afresh: the history above it once, then what is
//! on it now.
```

Move the Graphics tests from `rl-detect/src/ansi.rs` (`a_screen_is_handed_the_box_drawing_rather_than_the_letters` and any other test that names `Graphics`) into `screen.rs`'s `#[cfg(test)] mod tests`. In `ansi.rs`, `use rl_session::screen::dec_graphic;` and delete the local copy. Point `tui/term.rs` at `rl_session::screen::Graphics`.

Run: `cargo test -p rl-detect -p rl-session --lib && cargo build -p rl-cli`
Expected: PASS / builds.

- [ ] **Step 2: Write the failing `Screen` tests**

In `screen.rs` tests:

```rust
/// What a terminal shows after being handed `bytes`: its history, then its
/// screen, as plain text rows with trailing blanks trimmed.
fn shown(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
    let mut p = vt100::Parser::new(rows, cols, 10_000);
    p.process(bytes);
    let s = p.screen_mut();
    s.set_scrollback(usize::MAX);
    let back = s.scrollback();
    let mut out = Vec::new();
    // Oldest first: page down through the history a screenful at a time.
    let mut at = back;
    while at > 0 {
        s.set_scrollback(at);
        let take = at.min(rows as usize);
        out.extend(s.rows(0, cols).take(take).map(|r| r.trim_end().to_string()));
        at -= take;
    }
    s.set_scrollback(0);
    out.extend(s.rows(0, cols).map(|r| r.trim_end().to_string()));
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

#[test]
fn snapshot_round_trips_history_and_screen() {
    let mut s = Screen::new(20, 5);
    for i in 0..30 {
        s.feed(format!("line {i:02}\r\n").as_bytes());
    }
    s.feed(b"\x1b[1;31mred\x1b[m prompt");
    let want = shown(&{
        let mut all = Vec::new();
        for i in 0..30 {
            all.extend_from_slice(format!("line {i:02}\r\n").as_bytes());
        }
        all.extend_from_slice(b"\x1b[1;31mred\x1b[m prompt");
        all
    }, 20, 5);
    assert_eq!(shown(&s.snapshot(), 20, 5), want);
    assert_eq!(want.first().unwrap(), "line 00");
    assert_eq!(want.last().unwrap(), "red prompt");
}

#[test]
fn a_line_redrawn_in_place_is_drawn_once() {
    let mut s = Screen::new(40, 6);
    s.feed(b"header\r\n");
    for i in 0..200 {
        // Ink's way: up a line, erase it, write it again.
        s.feed(format!("\x1b[1A\x1b[2K\rframe {i:03}\r\n").as_bytes());
    }
    let snap = String::from_utf8_lossy(&s.snapshot()).into_owned();
    assert!(snap.contains("frame 199"), "{snap:?}");
    assert!(!snap.contains("frame 198"), "{snap:?}");
    assert_eq!(snap.matches("header").count(), 1, "{snap:?}");
}

#[test]
fn an_alternate_screen_app_is_drawn_on_the_alternate_screen() {
    let mut s = Screen::new(20, 4);
    s.feed(b"shell\r\n\x1b[?1049h\x1b[Hfull screen");
    let snap = s.snapshot();
    let text = String::from_utf8_lossy(&snap);
    let alt = text.find("\x1b[?1049h").expect("switches to the alternate screen");
    assert!(alt < text.find("full screen").unwrap());
    let mut p = vt100::Parser::new(4, 20, 0);
    p.process(&snap);
    assert!(p.screen().alternate_screen());
    assert!(p.screen().contents().contains("full screen"));
}

#[test]
fn the_box_drawing_set_survives_the_snapshot() {
    let mut s = Screen::new(10, 2);
    s.feed(b"\x1b(0qqq\x1b(Bok");
    let snap = String::from_utf8_lossy(&s.snapshot()).into_owned();
    assert!(snap.contains("───ok"), "{snap:?}");
}

#[test]
fn a_resize_is_the_size_the_snapshot_draws() {
    let mut s = Screen::new(10, 3);
    s.feed(b"abc");
    s.resize(30, 8);
    let mut p = vt100::Parser::new(8, 30, 0);
    p.process(&s.snapshot());
    assert_eq!(p.screen().size(), (8, 30));
    assert!(p.screen().contents().starts_with("abc"));
    assert_eq!(p.screen().cursor_position(), (0, 3));
}

#[test]
fn render_is_a_fresh_screen_fed_and_drawn() {
    let bytes = b"one\r\ntwo\r\n\x1b[1A\x1b[2Ktwo again";
    let mut s = Screen::new(20, 4);
    s.feed(bytes);
    assert_eq!(Screen::render(bytes, 20, 4), s.snapshot());
}
```

Run: `cargo test -p rl-session --lib screen`
Expected: FAIL to compile (`Screen` not defined).

- [ ] **Step 3: Implement `Screen`**

```rust
/// Rows of history kept above the screen. 32 bytes a cell: 2000 rows of 200
/// columns is about 12 MiB, the most one run holds; tmux keeps as many.
pub const HISTORY_ROWS: usize = 2000;

/// One run's screen, fed every byte it writes.
pub struct Screen {
    parser: vt100::Parser,
    graphics: Graphics,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), HISTORY_ROWS),
            graphics: Graphics::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(&self.graphics.rewrite(bytes));
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
    }

    /// Bytes that draw this screen on a terminal of its size: the history
    /// first, a line at a time, pushed up out of sight; then the screen
    /// itself, cleared and drawn, on the alternate screen if the app is on
    /// it; then the cursor, colours and input modes the app left set.
    pub fn snapshot(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let (rows, cols) = self.parser.screen().size();
        let screen = self.parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let mut at = screen.scrollback();
        let had_history = at > 0;
        // Oldest first: a viewed page starts `at` rows back.
        while at > 0 {
            screen.set_scrollback(at);
            let take = at.min(rows as usize);
            for row in screen.rows_formatted(0, cols).take(take) {
                out.extend_from_slice(&row);
                out.extend_from_slice(b"\x1b[m\r\n");
            }
            at -= take;
        }
        screen.set_scrollback(0);
        if had_history {
            // Up and out of the way, into the viewer's own scrollback,
            // before the screen is cleared and drawn where they stood.
            out.extend(std::iter::repeat_n(b'\n', rows.saturating_sub(1) as usize));
        }
        if screen.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(&screen.state_formatted());
        out
    }

    /// An ended run's screen, from as much of its output as there is.
    pub fn render(bytes: &[u8], cols: u16, rows: u16) -> Vec<u8> {
        let mut s = Self::new(cols, rows);
        s.feed(bytes);
        s.snapshot()
    }
}
```

If `rows_formatted` turns out to emit cursor positioning that breaks the line-at-a-time history, or `repeat_n` is not stable on this toolchain, adjust the code (for example `std::iter::repeat(b'\n').take(n)`) and keep the tests as written; the round-trip test is the arbiter.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rl-session --lib && cargo test -p rl-detect && cargo clippy -p rl-session -p rl-detect -p rl-cli --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add -A code/rl-session code/rl-detect code/rl-cli/src/tui/term.rs Cargo.toml Cargo.lock
git commit -m "rl-session keeps a run's screen: a headless vt100 and the bytes that draw it afresh"
```

---

### Task 2: The holder keeps the screen and hands it to an attach that asks

**Files:**
- Modify: `code/rl-session/src/proto.rs`: `Frame::Attach` gains `screen: bool`; `Hello` gains `screen: bool`; encode/decode; proto tests.
- Modify: `code/rl-session/src/holder.rs`: `Shared` gains `screen: Option<Screen>` (tty only); fed in `pump_output` under the existing lock; resized in the client `Resize` arm after the pty resize succeeds; `client_loop` answers a screen attach with the snapshot.
- Modify: `code/rl-session/src/client.rs`: `Session::attach_screen`.
- Test: `code/rl-session/tests/holder.rs`

**Interfaces:**
- Consumes: `rl_session::screen::Screen` (Task 1).
- Produces:
  - `Frame::Attach { replay: bool, from: u64, screen: bool }`
  - `Hello.screen: bool` (`#[serde(default)]`): true when the replay that follows is the screen drawn afresh, not bytes.
  - `Session::attach_screen(socket: &Path, from: u64) -> Result<(Hello, Session)>`: asks for the screen. A holder too old to keep one replays bytes from `from` instead, and its `hello.screen` is false.

- [ ] **Step 1: Failing proto tests**

In `proto.rs` tests add:

```rust
#[test]
fn attach_asks_for_the_screen_in_a_way_an_old_holder_reads_as_a_replay() {
    let f = Frame::Attach { replay: false, from: 42, screen: true };
    let bytes = f.encode();
    // The flag byte: an old holder reads any non-zero one as "replay".
    assert_eq!(bytes[5], 2);
    assert_ne!(bytes[5], 0);
    assert_eq!(roundtrip(&f), Frame::Attach { replay: true, from: 42, screen: true });
    let plain = Frame::Attach { replay: true, from: 7, screen: false };
    assert_eq!(plain.encode()[5], 1);
    assert_eq!(roundtrip(&plain), plain);
}
```

(Use the test module's existing round-trip helper, or encode then read back with `read_frame` as the existing tests do. Update every existing `Frame::Attach { .. }` literal in the tests to carry `screen: false`.) A screen request decodes with `replay: true` because a screen attach falls back to a byte replay on a holder without a screen.

Run: `cargo test -p rl-session --lib proto` → FAIL (no field `screen`).

- [ ] **Step 2: Implement the frame and hello changes**

Encoding: flag byte = `if screen { 2 } else { replay as u8 }`. Decoding: `let flag = p.first().copied().unwrap_or(0); replay: flag != 0, screen: flag & 2 != 0`. Document on the variant:

```rust
/// Must be the first frame on a connection. `replay` asks for the ring
/// buffer before live output; `from` is the byte offset (in the child's
/// total output) to replay from — `0` means everything the ring still has.
/// `screen` asks instead for the screen drawn afresh (a tty run's): sent as
/// a flag an older holder reads as a plain `replay`, so it answers with bytes
/// from `from` and a `Hello` whose `screen` is false.
```

`Hello` gets:

```rust
/// The replay that follows is the screen drawn afresh, not bytes.
#[serde(default)]
pub screen: bool,
```

and `Shared::hello` sets `screen: false`.

- [ ] **Step 3: Failing holder test**

In `code/rl-session/tests/holder.rs`, using its `start`/`read_until` helpers and `Session::attach_screen`:

```rust
#[tokio::test]
async fn a_screen_attach_draws_the_screen_once() {
    let h = start(true, 5);
    let (_, mut s) = attach(&h, false).await;
    // A counter redrawn in place: every value is in the bytes, only the
    // last is on the screen.
    s.input(b"i=0; while [ $i -lt 100 ]; do printf 'n=%03d\\r' $i; i=$((i+1)); done; echo; echo drawn\n".to_vec())
        .await
        .unwrap();
    read_until(&mut s, "drawn").await;
    let (hello, mut v) = Session::attach_screen(&h.socket, 0).await.unwrap();
    assert!(hello.screen);
    assert_eq!(hello.replay_from, hello.total);
    let mut drawn = Vec::new();
    loop {
        match v.recv().await.unwrap() {
            Some(Frame::Output(b)) => drawn.extend(b),
            Some(Frame::ReplayEnd) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    let drawn = String::from_utf8_lossy(&drawn);
    assert!(drawn.contains("n=099"), "{drawn:?}");
    assert!(!drawn.contains("n=098"), "{drawn:?}");
}

#[tokio::test]
async fn a_pipes_run_answers_a_screen_attach_with_bytes() {
    let h = start(false, 5);
    let (_, mut s) = attach(&h, false).await;
    s.input(b"echo piped\n".to_vec()).await.unwrap();
    read_until(&mut s, "piped").await;
    let (hello, mut v) = Session::attach_screen(&h.socket, 0).await.unwrap();
    assert!(!hello.screen);
    assert!(read_until(&mut v, "piped").await.contains("piped"));
}
```

Adjust to the helper names actually in the file (`h.socket`, the fake CLI's echo behaviour, how input reaches a shell). The fake CLI used by `start` may not be a shell. If so, pass a shell argv through `start_with` the way other tests in the file do. The assertions are what matter.

Run: `cargo test --workspace -p rl-session --test holder` → FAIL.

- [ ] **Step 4: Implement the holder side**

- `Shared { screen: Option<Screen>, .. }`, created in `serve` as `cfg.spec.tty.then(|| Screen::new(cfg.spec.cols, cfg.spec.rows))`.
- `pump_output`: under the lock, `if let Some(sc) = s.screen.as_mut() { sc.feed(&chunk); }` next to `s.ring.push`. Update the doc comment above `pump_output`, which currently sits on `signal_tree` by mistake: move "Child output → ring + broadcast …" back onto `pump_output` and say the screen is fed under that lock too, so a snapshot and the subscription line up with no gap and no duplicate.
- `Resize` arm: after `(ctx.resize)` succeeds, inside the same lock, also `if let Some(sc) = s.screen.as_mut() { sc.resize(cols, rows); }`.
- `client_loop`: read `Attach { replay, from, screen }`. Under the lock: if `screen` and `s.screen` is `Some`, set `hello.screen = true`, `hello.replay_from = total`, `snapshot = sc.snapshot()`. Otherwise, if `replay`, do today's ring tail. Everything after (write Hello, the Output if non-empty, ReplayEnd, Exit) stays as is.
- `client.rs`: `attach_from` builds `Frame::Attach { replay: from.is_some(), from: from.unwrap_or(0), screen: false }`. Add:

```rust
/// Attaches asking for the screen drawn afresh. A holder that keeps none
/// (a pipes run, or one started by an older binary) replays bytes from
/// `from` instead, and says so: `hello.screen` is false.
pub async fn attach_screen(socket: &Path, from: u64) -> Result<(Hello, Self)> {
```

Factor the shared connect-send-hello into a private `fn open(socket, attach: Frame)` so `attach_from` and `attach_screen` are each a line.
- Update the module docs in `holder.rs` and `lib.rs` to mention the screen alongside the ring buffer.

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git commit -am "The holder keeps its run's screen and hands it to an attach that asks"
```

---

### Task 3: `rld` hands viewers the screen, and the nudge goes

**Files:**
- Modify: `code/rl-daemon/src/attach.rs`
- Modify: `code/rl-daemon/src/supervisor.rs` (delete the `nudge` field and `nudge_start`, `nudged_size`, `nudge_resized`, `nudge_end`, and the `nudge: Mutex::new(None)` initialiser)
- Modify: `code/rl-proto/src/methods.rs` (docs on `RunAttach`, `AttachParams::scrollback`, `AttachInfo::journaled`/`replay_from`)
- Modify: `code/rl-cli/src/main.rs:151` (doc for `--scrollback`, if it says "replay")
- Modify: `docs/ARCHITECTURE.md` §4 (diagram and the two nudge paragraphs)
- Test: `code/rl-daemon/tests/attach.rs`

**Interfaces:**
- Consumes: `Session::attach_screen`, `Hello.screen`, `Screen::render` (Tasks 1–2).

- [ ] **Step 1: Rewrite the tests first**

In `code/rl-daemon/tests/attach.rs`:

- `watchers_and_one_joiner`: remove the nudge wording in comments ("so the one nudge is over by the time it asks", "The post-replay repaint nudge leaves the pty exactly as big as it was …") and the `sleep(800ms)` that waited for it. Keep `past_young()` before the join, since `YOUNG` still exists. The existing assertions stay (`replay_from == journaled`: no gap is reported for a screen attach).
- Delete `a_join_makes_the_run_see_a_resize_it_can_act_on` and `letting_go_leaves_the_run_a_row_short_for_the_next_join`.
- Keep `a_join_at_a_new_size_is_one_repaint` and `a_run_joined_before_it_listens_still_hears_its_size`, with comment wording that doesn't refer to the removed nudge.
- Add:

```rust
/// Joining at the size the run has and letting go again is nothing it hears:
/// what a joiner is handed is the screen, so nothing has to make it repaint.
#[tokio::test]
async fn joining_and_letting_go_at_the_runs_size_is_no_resize() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", SIZES], "cwd": tmp.path().to_string_lossy() }),
        )
        .await;
    wait_journal(&h.home, &run.id, "ready").await;
    past_young().await;
    let (info, mut joiner) = Viewer::attach(&h.home, &run.id, "join", "me@here", false)
        .await
        .unwrap();
    let (cols, rows) = (info["cols"].as_u64().unwrap() as u16, info["rows"].as_u64().unwrap() as u16);
    resize(&mut joiner, cols, rows).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(joiner);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sizes_heard(&h.home, &run.id, tmp.path()).await.is_empty());
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    h.stop().await;
}

/// A watcher coming to a run that redrew one line a hundred times is handed
/// the line as it stands, once, not every redraw on the way.
#[tokio::test]
async fn a_watcher_sees_the_screen_not_every_redraw() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let script = r#"i=0; while [ $i -lt 100 ]; do printf 'n=%03d\r' $i; i=$((i+1)); done; echo; echo drawn; while :; do sleep 1; done"#;
    let run: Run = c
        .ok("run.start", json!({ "argv": ["/bin/sh", "-c", script], "cwd": tmp.path().to_string_lossy() }))
        .await;
    wait_journal(&h.home, &run.id, "drawn").await;
    let (info, mut v) = Viewer::attach(&h.home, &run.id, "watch", "ann@laptop", false)
        .await
        .unwrap();
    assert_eq!(info["live"], true);
    v.until("drawn").await;
    assert!(v.seen.contains("n=099"), "{}", v.seen);
    assert!(!v.seen.contains("n=098"), "{}", v.seen);
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    let done = c.wait_terminal(&run.id).await;
    let _ = done;
    // Ended, the same: drawn from the journal, not replayed from it.
    let (info, mut late) = Viewer::attach(&h.home, &run.id, "watch", "ann@laptop", false)
        .await
        .unwrap();
    assert_eq!(info["live"], false);
    late.until("drawn").await;
    assert!(late.seen.contains("n=099"), "{}", late.seen);
    assert!(!late.seen.contains("n=098"), "{}", late.seen);
    h.stop().await;
}
```

Note that the test viewer requests `scrollback: 4096`, and the ended-run render feeds that journal tail; 100 × 6 bytes fits.

Run: `cargo test --workspace -p rl-daemon --test attach` → the new tests FAIL (the watcher sees `n=098`).

- [ ] **Step 2: Implement in `attach.rs`**

- `attach()`, live branch: `Session::attach_screen(&live.socket, journaled)` when the run is a tty run, otherwise `Session::attach_from(&live.socket, Some(journaled))` as today. The run's tty flag comes from `sessions::get` (read in the same ledger call as the tail) or from the run row if it carries it. Pick the one that exists and don't add a new query if the data is already at hand. If `hello.screen`, the journal tail is not sent (`tail = Vec::new()`) and `AttachInfo.replay_from = journaled` (no gap: the screen is whole). Otherwise keep today's values.
- `attach()`, ended branch: when `tty` and `cols > 0`, replace `tail` with `Screen::render(&tail, cols, rows)`. Pipes runs keep the raw tail.
- `stream()`: delete `NUDGE_HOLD`'s use for the replay, `JOIN_SIZE_WAIT`, `first_size`, `replayed`, `size_wait`, the watcher nudge, the `ReplayEnd` nudge logic (keep forwarding `ReplayEnd`), and the "letting go leaves it a row short" block at the end. What remains for the joiner:
  - On `ReplayEnd`, if `a.info.tty && a.joiner && live.is_joiner(&a.seat)` and the run is younger than `YOUNG`, arm `again` for when it reaches that age.
  - When `again` fires and we are still the joiner, give the pty a row less, arm `restore` for `NUDGE_HOLD` later, and when that fires, give `size` back if still the joiner. All of this is local to the stream: no state in `Live`.
  - On exit, if `restore` is pending and we are still the joiner, give `size` back.
  - The `Resize` arm keeps `size = (cols, rows)`, `hwr.resize`, and `sessions::set_size`; delete the `live.nudge_resized` and `first_size` handling.
  - `size` starts as `(a.info.cols, a.info.rows)`.
- Replace the `nudge()` fn with a smaller one or inline it. Rewrite the doc comments of `NUDGE_HOLD`/`YOUNG` so they only describe the young-join case. Update the module doc ("scrollback from the journal, then the holder's replay …") to: a tty run's viewer is handed its screen drawn afresh by the holder; a pipes run's gets the journal's tail and the holder's replay from where the journal stops; an ended run's gets the journal's tail, drawn as a screen when it was a tty run.
- `supervisor.rs`: delete the `nudge` field, its four methods and its doc comment.

- [ ] **Step 3: Docs**

- `rl-proto/src/methods.rs`: `RunAttach` doc says what comes first per kind of run, as in the module doc above. `scrollback`: "Bytes of journaled output to draw from for an ended run or a pipes run; a live terminal run is drawn from the holder's own screen and history."
- `docs/ARCHITECTURE.md` §4: in the diagram, `D->>S: attach for the screen (a tty run) or with replay from where the journal stops (pipes)` and `S-->>D: Output (the screen drawn afresh, or bytes) … ReplayEnd`. Make `D-->>V: Output (scrollback)` conditional ("pipes, or an ended run: the journal tail, drawn as a screen for a tty run"). Replace the two paragraphs about nudging with one paragraph on why a screen and not bytes (the reasons from the `screen.rs` module doc, plus: an old holder without a screen gets the byte path), and one on the young-join nudge (`YOUNG`, `NUDGE_HOLD`, `spawn_size`).
- `docs/PRODUCT.md:125`: "Scrollback replay on attach from journaled `session_io`" becomes "On attach, a terminal run's screen and history drawn afresh by its holder; journaled `session_io` for pipes and ended runs."

- [ ] **Step 4: Sweep for dead code**

Run: `grep -rn "nudge\|nudged\|JOIN_SIZE_WAIT\|first_size\|size_wait\|row short\|Graphics" code docs --include='*.rs' --include='*.md'`. Every hit must be the young-join nudge, the moved `Graphics`, or a dated plan file. Run `cargo build --workspace` and check for `dead_code` warnings.

- [ ] **Step 5: Run everything**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git commit -am "Watching or joining a terminal run draws its screen once; the repaint nudge goes"
```

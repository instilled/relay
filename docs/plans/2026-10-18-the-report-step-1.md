# The Report · Step 1 — lead model, `rl lead post`/`clear`, built-in projection, `rl report`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the backend and CLI of "the report" — one owned `Lead` type, a `leads` store written by `rl lead post`/`clear`, a built-in projection of Relay's own state (open decisions + runs) into leads, and `rl report [scope] [since]` that prints needs-you / live / happened as text — with no UI.

**Architecture:** A `Lead` is the one object (`rl-proto`). User leads are rows in a new `leads` table keyed by `(source, key)`, so a re-emit refreshes the one lead and `clear` deletes it (source-owned, keyed lifecycle). Relay's own leads are **never stored** — they are projected at read time from the `decisions` and `runs` tables through the read-only pool (`call_read`), so they track the truth and self-clear. `report.get` gathers both, drops anything expired, groups into bands, and returns a `Report`; `rl report` renders it. A thin `leads` daemon service sweeps expired rows and is the seat the push hook (step 4) will later sit in; correctness does not depend on it because the read path filters expiry itself.

**Tech Stack:** Rust workspace (`code/`), SQLite via `rl-ledger` (single writer thread + 4-conn read pool), JSON-RPC over a Unix socket via the `rl_proto::Method` trait, `clap` derive CLI, `tokio` daemon, `jiff`-backed `rl_proto::Timestamp`.

**Spec:** `docs/specs/2026-10-17-the-report.md`

## Global Constraints

- **Build off `main`**, not `chore/roadmap`. `main` HEAD carries the read pool (`call_read`, PR #177), the `rows/` split (#182), and migrations through `0025_session_io_files`. One worktree, one PR for all of step 1.
- **CI gates, in order (a warning is an error):** `cargo fmt --all --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace`. Run all three before pushing.
- **rustfmt:** `max_width = 120`; top-level items close at a column-0 `}` or end with `;`.
- **No backward compatibility.** Bump `PROTOCOL_VERSION` (`code/rl-proto/src/lib.rs:18`) once in this PR; after merge the running `rl`/`rld` must be killed so the new protocol takes (daemon and CLI version-check must match). No fallbacks for old peers.
- **Ledger access:** reads go through `Ledger::call_read` (async) / `call_read_blocking` (sync, in tests); writes go through `Ledger::call` / `call_blocking`. In-memory ledgers have no pool and fall back to the writer — tests on `Ledger::open_in_memory` may use either.
- **Migrations are append-only.** Never edit a shipped `.sql`. The next number is **0026**; register it in the `MIGRATIONS` array and add the new table name to the migrate idempotence test's sorted table list.
- **Output convention:** every CLI handler takes `json: bool`; `if json { println!("{}", serde_json::to_string(&x)?) } else { <human output> }`. Human lists use `render::table(headers, &rows)`.
- **Reserved source:** `relay` is reserved for built-in leads. `rl lead post --source relay …` is refused, so user leads never shadow projected ones.
- **TDD throughout.** Watch each test fail before implementing.

## Review Focus

Inputs the spec implies but no happy-path task exercises — each pinned by a test in the task that owns it:

- **Unknown `--band` value** (`rl lead post --band urgent`) → a clear error naming the valid bands, not a panic or a garbage row. *(Task 1: `Band::from_str` rejects; Task 6: CLI surfaces it.)*
- **Re-emit under the same `(source, key)`** → the one lead is refreshed in place (band/title/at updated), never duplicated. *(Task 2: upsert test.)*
- **`clear` on a `(source, key)` that was never posted** → silent success, idempotent. *(Task 2: clear-missing test.)*
- **An expired lead must never appear in the report**, even if the sweep task has not run yet. *(Task 2: `list` excludes expired; Task 4: `assemble` drops expired defensively.)*
- **`since` bounds only the `happened` band** — an old open decision or a long-running job still shows under needs-you / live regardless of `since`. *(Task 4: `assemble` applies `since` to happened only.)*

---

## File Structure

- `code/rl-proto/src/lead.rs` — **new.** Owned `Lead`, `Band`, `LeadStatus`. The wire type shared by CLI, daemon, ledger.
- `code/rl-proto/src/methods.rs` — **modify.** Add `ReportGet`, `LeadPost`, `LeadClear` method markers + param/result structs.
- `code/rl-proto/src/lib.rs` — **modify.** `pub mod lead;`, re-export, bump `PROTOCOL_VERSION`.
- `code/rl-ledger/migrations/0026_leads.sql` — **new.** The `leads` table.
- `code/rl-ledger/src/leads.rs` — **new.** `post` / `clear` / `list` / `sweep_expired` accessors.
- `code/rl-ledger/src/lib.rs` — **modify.** `pub mod leads;`.
- `code/rl-ledger/src/migrate.rs` — **modify.** Register `0026`; extend the table-list test.
- `code/rl-daemon/src/report.rs` — **new.** `project_builtins(...)` and `assemble(...)` — pure functions.
- `code/rl-daemon/src/leads.rs` — **new.** The thin `Leads` service (sweep + push seat).
- `code/rl-daemon/src/handlers.rs` — **modify.** Register `report.get`, `lead.post`, `lead.clear`.
- `code/rl-daemon/src/server.rs` — **modify.** Spawn `leads.serve`; add `leads` field to `Daemon`.
- `code/rl-daemon/src/lib.rs` — **modify.** `mod report; mod leads;`.
- `code/rl-cli/src/main.rs` — **modify.** `Report(ReportArgs)` and `Lead { cmd: LeadCmd }` variants + handlers.
- `code/rl-cli/src/render.rs` — **modify.** Lead-row builders for the report table.

---

## Task 1: The owned `Lead` type (`rl-proto`)

**Files:**
- Create: `code/rl-proto/src/lead.rs`
- Modify: `code/rl-proto/src/lib.rs` (add `pub mod lead;` beside the other `pub mod`s, ~lib.rs:1-30)
- Test: inline `#[cfg(test)]` in `code/rl-proto/src/lead.rs`

**Interfaces:**
- Produces: `rl_proto::lead::Lead` (struct, fields below), `rl_proto::lead::Band` (`NeedsYou | Live | Happened`, `fn rank(self) -> u8`, `fn as_str(self) -> &'static str`, `FromStr`), `rl_proto::lead::LeadStatus` (`Open | Resolved | Dismissed`).

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-proto/src/lead.rs  (bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn band_parses_and_ranks() {
        assert_eq!(Band::from_str("needs_you").unwrap(), Band::NeedsYou);
        assert_eq!(Band::from_str("live").unwrap(), Band::Live);
        assert_eq!(Band::from_str("happened").unwrap(), Band::Happened);
        assert!(Band::NeedsYou.rank() < Band::Live.rank());
        assert!(Band::Live.rank() < Band::Happened.rank());
    }

    #[test]
    fn band_rejects_unknown() {
        let err = Band::from_str("urgent").unwrap_err();
        assert!(err.contains("needs_you"), "error names the valid bands: {err}");
    }

    #[test]
    fn lead_round_trips_json() {
        let l = Lead {
            source: "feedback".into(),
            key: "notion:triage".into(),
            band: Band::NeedsYou,
            title: "5 feedback items to triage".into(),
            summary: None,
            scope: None,
            action: Some("claude -p 'triage the feedback queue'".into()),
            status: LeadStatus::Open,
            at: Timestamp::now(),
            expires_at: None,
        };
        let s = serde_json::to_string(&l).unwrap();
        let back: Lead = serde_json::from_str(&s).unwrap();
        assert_eq!(l, back);
        assert!(s.contains("\"needs_you\""), "band serializes snake_case");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-proto lead::tests`
Expected: FAIL — `Lead`, `Band`, `LeadStatus` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// code/rl-proto/src/lead.rs  (top)
//! A lead: one thing worth your attention, from one source. The one object of
//! the report — projected from Relay's own state, or posted by a flow.

use crate::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

/// What ranks a lead. The only thing that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// Cannot proceed without you. Pinned to the top.
    NeedsYou,
    /// Happening now, may change.
    Live,
    /// Done, here for the record.
    Happened,
}

impl Band {
    pub fn rank(self) -> u8 {
        match self {
            Band::NeedsYou => 0,
            Band::Live => 1,
            Band::Happened => 2,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Band::NeedsYou => "needs_you",
            Band::Live => "live",
            Band::Happened => "happened",
        }
    }
}

impl FromStr for Band {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "needs_you" | "needs-you" => Ok(Band::NeedsYou),
            "live" => Ok(Band::Live),
            "happened" => Ok(Band::Happened),
            other => Err(format!("unknown band {other:?} — use one of: needs_you, live, happened")),
        }
    }
}

/// Where a lead stands. User leads are `Open` until cleared; a dismiss is a
/// user action from the pane (step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadStatus {
    Open,
    Resolved,
    Dismissed,
}

/// A single thing worth your attention, from one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lead {
    /// Who raised it — a flow name, an `rl lead` source, or `relay` for built-ins.
    pub source: String,
    /// The source's own id for this lead, stable across re-emits.
    pub key: String,
    pub band: Band,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// A project/folder this is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PathBuf>,
    /// How to act on it — a command to run or a thing to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    pub status: LeadStatus,
    /// Created / last updated.
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}
```

Then add to `code/rl-proto/src/lib.rs`, beside the existing `pub mod decision;` / `pub mod flow;` declarations:

```rust
pub mod lead;
```

(Confirm `Timestamp` is exported from the crate root as the other modules import it — `use crate::Timestamp;`. If the crate root re-exports it under a different path, match what `decision.rs` uses.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rl-proto lead::tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add code/rl-proto/src/lead.rs code/rl-proto/src/lib.rs
git commit -m "The report: the owned Lead type, bands, status"
```

---

## Task 2: The `leads` ledger table + accessors (`rl-ledger`)

**Files:**
- Create: `code/rl-ledger/migrations/0026_leads.sql`
- Create: `code/rl-ledger/src/leads.rs`
- Modify: `code/rl-ledger/src/lib.rs` (add `pub mod leads;` in the `pub mod` block, ~lib.rs:9-23)
- Modify: `code/rl-ledger/src/migrate.rs` (register `0026`; extend the table-list test, ~migrate.rs:48 and ~migrate.rs:84-120)
- Test: inline `#[cfg(test)]` in `code/rl-ledger/src/leads.rs`

**Interfaces:**
- Consumes: `rl_proto::lead::{Lead, Band, LeadStatus}` (Task 1); `Ledger::open_in_memory`, `Ledger::call_blocking` (existing); `rl_core::time::{to_millis, from_millis}` and `rl_proto::Timestamp` (existing idiom — mirror `code/rl-ledger/src/decisions.rs:221-254` `from_row` exactly for the millis↔Timestamp conversion).
- Produces:
  - `rl_ledger::leads::post(conn: &Connection, l: &Lead) -> Result<()>` — upsert on `(source, key)`.
  - `rl_ledger::leads::clear(conn: &Connection, source: &str, key: &str) -> Result<()>` — delete, idempotent.
  - `rl_ledger::leads::list(conn: &Connection, now: Timestamp) -> Result<Vec<Lead>>` — all non-expired user leads, newest first.
  - `rl_ledger::leads::sweep_expired(conn: &Connection, now: Timestamp) -> Result<usize>` — delete expired, return count.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-ledger/src/leads.rs  (bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;
    use rl_proto::lead::{Band, Lead, LeadStatus};
    use rl_proto::Timestamp;

    fn lead(source: &str, key: &str, band: Band, at: Timestamp) -> Lead {
        Lead {
            source: source.into(),
            key: key.into(),
            band,
            title: format!("{source}/{key}"),
            summary: None,
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
        }
    }

    #[test]
    fn post_then_list() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        led.call_blocking(move |c| post(c, &lead("feedback", "triage", Band::NeedsYou, now))).unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "feedback");
        assert_eq!(out[0].band, Band::NeedsYou);
    }

    #[test]
    fn re_post_refreshes_in_place() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        led.call_blocking(move |c| post(c, &lead("gh", "pr:412", Band::Live, now))).unwrap();
        let mut again = lead("gh", "pr:412", Band::NeedsYou, now);
        again.title = "review requested on #412".into();
        led.call_blocking(move |c| post(c, &again)).unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert_eq!(out.len(), 1, "same (source,key) is one lead, not two");
        assert_eq!(out[0].band, Band::NeedsYou);
        assert_eq!(out[0].title, "review requested on #412");
    }

    #[test]
    fn clear_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        // Clearing something never posted is fine.
        led.call_blocking(move |c| clear(c, "gh", "pr:999")).unwrap();
        led.call_blocking(move |c| post(c, &lead("gh", "pr:412", Band::Live, now))).unwrap();
        led.call_blocking(move |c| clear(c, "gh", "pr:412")).unwrap();
        led.call_blocking(move |c| clear(c, "gh", "pr:412")).unwrap(); // again, still fine
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn expired_never_listed_and_sweeps() {
        let led = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        let past = now - std::time::Duration::from_secs(60);
        let mut l = lead("cron", "nightly", Band::Happened, past);
        l.expires_at = Some(past); // already expired
        led.call_blocking(move |c| post(c, &l)).unwrap();
        let out = led.call_blocking(move |c| list(c, now)).unwrap();
        assert!(out.is_empty(), "expired lead is never listed even before sweep");
        let n = led.call_blocking(move |c| sweep_expired(c, now)).unwrap();
        assert_eq!(n, 1);
    }
}
```

(If `Timestamp` arithmetic (`now - Duration`) is not available, build `past` the way `decisions.rs` tests do — mirror an existing ledger test that constructs an earlier `Timestamp`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-ledger leads::tests`
Expected: FAIL — module `leads` / `post` not found.

- [ ] **Step 3: Write minimal implementation**

Migration `code/rl-ledger/migrations/0026_leads.sql`:

```sql
-- User-posted leads: one row per (source, key). Re-posting refreshes in place;
-- clearing deletes. Relay's own leads are projected at read time, never stored.
CREATE TABLE leads (
    source      TEXT    NOT NULL,
    key         TEXT    NOT NULL,
    band        TEXT    NOT NULL,          -- needs_you | live | happened
    title       TEXT    NOT NULL,
    summary     TEXT,
    scope       TEXT,                       -- a folder path
    action      TEXT,
    status      TEXT    NOT NULL,           -- open | resolved | dismissed
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    expires_at  INTEGER,                    -- epoch millis, NULL = no TTL
    PRIMARY KEY (source, key)
);

CREATE INDEX leads_band_idx ON leads (band, updated_at DESC);
CREATE INDEX leads_expires_idx ON leads (expires_at);
```

Register it in `code/rl-ledger/src/migrate.rs` — add to the `MIGRATIONS` array after the `0025` tuple:

```rust
    (26, "0026_leads", include_str!("../migrations/0026_leads.sql")),
```

And add `"leads"` to the sorted table list asserted in the idempotence test (~migrate.rs:84-120), keeping alphabetical order (it falls between `hidden`/`firings` grouping and `meta` — insert so the list stays exactly sorted as the test asserts).

`code/rl-ledger/src/leads.rs`:

```rust
//! The `leads` store: user-posted leads, one row per (source, key). Relay's own
//! leads are projected at read time and never land here.

use crate::Result;
use rl_core::time::{from_millis, to_millis};
use rl_proto::lead::{Band, Lead, LeadStatus};
use rl_proto::Timestamp;
use rusqlite::Connection;
use std::path::PathBuf;

const COLS: &str = "source, key, band, title, summary, scope, action, status, created_at, updated_at, expires_at";

/// Upsert a lead by its (source, key). A re-post refreshes everything but the
/// original `created_at`.
pub fn post(conn: &Connection, l: &Lead) -> Result<()> {
    conn.execute(
        "INSERT INTO leads (source, key, band, title, summary, scope, action, status, created_at, updated_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)
         ON CONFLICT(source, key) DO UPDATE SET
            band = excluded.band, title = excluded.title, summary = excluded.summary,
            scope = excluded.scope, action = excluded.action, status = excluded.status,
            updated_at = excluded.updated_at, expires_at = excluded.expires_at",
        rusqlite::params![
            l.source,
            l.key,
            l.band.as_str(),
            l.title,
            l.summary,
            l.scope.as_ref().map(|p| p.to_string_lossy().into_owned()),
            l.action,
            status_str(l.status),
            to_millis(l.at),
            l.expires_at.map(to_millis),
        ],
    )?;
    Ok(())
}

/// Delete a lead. Missing is not an error.
pub fn clear(conn: &Connection, source: &str, key: &str) -> Result<()> {
    conn.execute("DELETE FROM leads WHERE source = ?1 AND key = ?2", rusqlite::params![source, key])?;
    Ok(())
}

/// Every non-expired user lead, newest first.
pub fn list(conn: &Connection, now: Timestamp) -> Result<Vec<Lead>> {
    let sql = format!(
        "SELECT {COLS} FROM leads WHERE expires_at IS NULL OR expires_at > ?1 ORDER BY updated_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![to_millis(now)], from_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
}

/// Delete expired leads; return how many went.
pub fn sweep_expired(conn: &Connection, now: Timestamp) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM leads WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        rusqlite::params![to_millis(now)],
    )?;
    Ok(n)
}

fn status_str(s: LeadStatus) -> &'static str {
    match s {
        LeadStatus::Open => "open",
        LeadStatus::Resolved => "resolved",
        LeadStatus::Dismissed => "dismissed",
    }
}

fn status_from(s: &str) -> LeadStatus {
    match s {
        "resolved" => LeadStatus::Resolved,
        "dismissed" => LeadStatus::Dismissed,
        _ => LeadStatus::Open,
    }
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lead> {
    let band: String = row.get(2)?;
    let status: String = row.get(7)?;
    let scope: Option<String> = row.get(5)?;
    let updated: i64 = row.get(9)?;
    let expires: Option<i64> = row.get(10)?;
    Ok(Lead {
        source: row.get(0)?,
        key: row.get(1)?,
        band: band.parse().unwrap_or(Band::Happened),
        title: row.get(3)?,
        summary: row.get(4)?,
        scope: scope.map(PathBuf::from),
        action: row.get(6)?,
        status: status_from(&status),
        at: from_millis(updated),
        expires_at: expires.map(from_millis),
    })
}
```

(Note: `at` is stored into both `created_at` and `updated_at` on first insert via the repeated `?9`; `list`/`from_row` surface `updated_at` as `Lead.at`, so a re-post bumps the visible time. If `crate::Result` / `rusqlite` imports differ from the siblings, copy the exact `use` lines from `code/rl-ledger/src/decisions.rs:1-15`. Confirm `from_millis`/`to_millis` signatures against `decisions.rs`.)

Add to `code/rl-ledger/src/lib.rs`, in the `pub mod` block:

```rust
pub mod leads;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rl-ledger leads::tests` then `cargo test -p rl-ledger migrate`
Expected: PASS (4 leads tests + the migrate idempotence test with `leads` in the table list).

- [ ] **Step 5: Commit**

```bash
git add code/rl-ledger/migrations/0026_leads.sql code/rl-ledger/src/leads.rs code/rl-ledger/src/lib.rs code/rl-ledger/src/migrate.rs
git commit -m "The report: the leads table and its post/clear/list/sweep accessors"
```

---

## Task 3: The built-in projection (`rl-daemon`)

**Files:**
- Create: `code/rl-daemon/src/report.rs`
- Modify: `code/rl-daemon/src/lib.rs` (add `mod report;`)
- Test: inline `#[cfg(test)]` in `code/rl-daemon/src/report.rs`

**Interfaces:**
- Consumes: `rl_proto::decision::{Decision, DecisionStatus}`, `rl_proto::types::{Run, RunState}`, `rl_proto::flow::Firing`, `rl_proto::lead::{Lead, Band, LeadStatus}`, `rl_proto::Timestamp`.
- Produces: `report::project_builtins(decisions: &[Decision], runs: &[Run], firings: &[Firing]) -> Vec<Lead>`. Source `"relay"`; keys `decision:<id>` / `run:<id>` / `firing:<id>`.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-daemon/src/report.rs  (bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::lead::Band;
    use rl_proto::types::RunState;

    // Build test fixtures with the crates' own constructors / Default where they
    // exist; mirror an existing rl-daemon test that makes a Run and a Decision
    // (e.g. in decisions.rs tests) for the exact field set.

    #[test]
    fn open_decision_is_needs_you_with_decide_action() {
        let d = test_decision("d1", "approve the task?");
        let leads = project_builtins(&[d], &[], &[]);
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].band, Band::NeedsYou);
        assert_eq!(leads[0].source, "relay");
        assert_eq!(leads[0].key, "decision:d1");
        assert_eq!(leads[0].action.as_deref(), Some("rl decide d1"));
    }

    #[test]
    fn run_state_maps_to_band() {
        let cases = [
            (RunState::NeedsInput, Band::NeedsYou),
            (RunState::Blocked, Band::NeedsYou),
            (RunState::Failed, Band::NeedsYou),
            (RunState::Working, Band::Live),
            (RunState::Starting, Band::Live),
            (RunState::Idle, Band::Live),
            (RunState::Finished, Band::Happened),
            (RunState::Stopped, Band::Happened),
        ];
        for (state, want) in cases {
            let r = test_run("r1", state);
            let leads = project_builtins(&[], &[r], &[]);
            assert_eq!(leads.len(), 1, "{state:?} should project one lead");
            assert_eq!(leads[0].band, want, "{state:?}");
            assert_eq!(leads[0].key, "run:r1");
        }
    }

    #[test]
    fn resting_shell_is_not_projected() {
        let shell = test_shell("sh1", RunState::Idle);
        assert!(project_builtins(&[], &[shell], &[]).is_empty(), "an idle shell is not news");
    }

    #[test]
    fn firing_band_by_whether_it_ended() {
        let live = test_firing("f1", None);
        let done = test_firing("f2", Some(Timestamp::now()));
        assert_eq!(project_builtins(&[], &[], &[live])[0].band, Band::Live);
        assert_eq!(project_builtins(&[], &[], &[done])[0].band, Band::Happened);
    }

    // --- fixtures: fill fields to match the real structs (copy from sibling tests) ---
    fn test_decision(id: &str, title: &str) -> rl_proto::decision::Decision { /* status Open */ unimplemented!() }
    fn test_run(id: &str, state: RunState) -> rl_proto::types::Run { unimplemented!() }
    fn test_shell(id: &str, state: RunState) -> rl_proto::types::Run { /* RunKind::Shell */ unimplemented!() }
    fn test_firing(id: &str, ended: Option<Timestamp>) -> rl_proto::flow::Firing { unimplemented!() }
}
```

> Implementer note: replace the four `unimplemented!()` fixtures with real constructions — copy the field set from an existing `rl-daemon`/`rl-proto` test that builds a `Run`, `Decision`, and `Firing` (the explore of `decisions.rs` tests and `types.rs` shows every field). A fixture that doesn't compile is not a placeholder in the plan's sense — the surrounding assertions are the spec of behavior; fill the fixtures to match the structs in front of you.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-daemon report::tests`
Expected: FAIL — `project_builtins` not defined (and fixtures `unimplemented!()`).

- [ ] **Step 3: Write minimal implementation**

```rust
// code/rl-daemon/src/report.rs  (top)
//! Projecting Relay's own state into leads, and assembling the report. Pure
//! functions over data the handler reads; no ledger, no I/O, so they test flat.

use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::Firing;
use rl_proto::lead::{Band, Lead, LeadStatus};
use rl_proto::types::{Run, RunState};
use rl_proto::Timestamp;

/// The source every built-in lead carries. Reserved — user leads may not use it.
pub const BUILTIN_SOURCE: &str = "relay";

/// Relay's own state as leads: open decisions, runs by state, flow firings.
/// Computed, never stored, so it tracks the truth and self-clears.
pub fn project_builtins(decisions: &[Decision], runs: &[Run], firings: &[Firing]) -> Vec<Lead> {
    let mut out = Vec::new();

    for d in decisions.iter().filter(|d| d.status == DecisionStatus::Open) {
        out.push(Lead {
            source: BUILTIN_SOURCE.into(),
            key: format!("decision:{}", d.id),
            band: Band::NeedsYou,
            title: d.title.clone(),
            summary: d.summary.clone(),
            scope: Some(d.workspace.clone()),
            action: Some(format!("rl decide {}", d.id)),
            status: LeadStatus::Open,
            at: d.created_at,
            expires_at: d.expires_at,
        });
    }

    for r in runs {
        // A shell at rest is not news — it is idle whenever you are not typing.
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
        out.push(Lead {
            source: BUILTIN_SOURCE.into(),
            key: format!("run:{}", r.id),
            band,
            title: r.title.clone(),
            summary: r.last_line.clone(),
            scope: Some(r.workspace.clone()),
            action: None, // acting on a run lead is step 2
            status: LeadStatus::Open,
            at: r.state_at,
            expires_at: None,
        });
    }

    for f in firings {
        let (band, at) = match f.ended_at {
            Some(end) => (Band::Happened, end),
            None => (Band::Live, f.started_at),
        };
        out.push(Lead {
            source: BUILTIN_SOURCE.into(),
            key: format!("firing:{}", f.id),
            band,
            title: format!("flow {}", f.flow),
            summary: f.outcome.clone(),
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
        });
    }

    out
}
```

Add `mod report;` to `code/rl-daemon/src/lib.rs`. (Confirm field names against the real structs: `Run.last_line`, `Run.state_at`, `Run.is_shell()`, `RunState::needs_attention()`/`is_terminal()`, `Decision.created_at`/`expires_at`/`workspace`, `Firing.ended_at`/`started_at`/`outcome`/`flow` — all named in the grounding. Fix any mismatch the compiler flags.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rl-daemon report::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add code/rl-daemon/src/report.rs code/rl-daemon/src/lib.rs
git commit -m "The report: project Relay's own state (decisions, runs, firings) into leads"
```

---

## Task 4: `assemble` + the `report.get` RPC

**Files:**
- Modify: `code/rl-daemon/src/report.rs` (add `assemble` + its tests)
- Modify: `code/rl-proto/src/methods.rs` (add `ReportGet`, `ReportParams`, `Report`)
- Modify: `code/rl-proto/src/lib.rs` (bump `PROTOCOL_VERSION`, lib.rs:18)
- Modify: `code/rl-daemon/src/handlers.rs` (register `report.get`, ~inside `register(r)` beside `DigestGet` at handlers.rs:419)
- Test: `assemble` tests inline in `report.rs`

**Interfaces:**
- Consumes: `project_builtins` (Task 3); `rl_ledger::leads::list` (Task 2); `rl_ledger::decisions::list` + `DecisionFilter`; `rl_ledger::runs::list_for_digest`; the firings gather used by the `DigestGet` handler (handlers.rs:419-437); `Ledger::call_read`.
- Produces: `report::assemble(builtins: Vec<Lead>, stored: Vec<Lead>, scope: Option<&Path>, since: Timestamp, now: Timestamp) -> Report`; `rl_proto::methods::{ReportGet, ReportParams, Report}`.
  - `Report { needs_you: Vec<Lead>, live: Vec<Lead>, happened: Vec<Lead> }`.
  - `ReportParams { scope: Option<PathBuf>, since: Option<Timestamp> }`.

- [ ] **Step 1: Write the failing test (assemble)**

```rust
// code/rl-daemon/src/report.rs  tests module — add:
    use rl_proto::methods::Report;
    use std::path::{Path, PathBuf};

    fn mk(band: Band, key: &str, at: Timestamp, scope: Option<&str>) -> Lead {
        Lead { source: "relay".into(), key: key.into(), band, title: key.into(), summary: None,
                scope: scope.map(PathBuf::from), action: None, status: LeadStatus::Open, at, expires_at: None }
    }

    #[test]
    fn since_bounds_only_happened() {
        let now = Timestamp::now();
        let old = now - std::time::Duration::from_secs(3600);
        let builtins = vec![
            mk(Band::NeedsYou, "decision:old", old, None), // old but needs-you: kept
            mk(Band::Live, "run:old", old, None),          // old but live: kept
            mk(Band::Happened, "run:recent", now, None),   // recent happened: kept
            mk(Band::Happened, "run:stale", old, None),    // old happened: dropped
        ];
        let since = now - std::time::Duration::from_secs(600);
        let r = assemble(builtins, vec![], None, since, now);
        assert_eq!(r.needs_you.len(), 1);
        assert_eq!(r.live.len(), 1);
        assert_eq!(r.happened.len(), 1, "old happened is bounded out, needs-you/live are not");
        assert_eq!(r.happened[0].key, "run:recent");
    }

    #[test]
    fn expired_dropped_defensively() {
        let now = Timestamp::now();
        let mut l = mk(Band::NeedsYou, "x", now, None);
        l.expires_at = Some(now - std::time::Duration::from_secs(1));
        let r = assemble(vec![], vec![l], None, now - std::time::Duration::from_secs(10), now);
        assert!(r.needs_you.is_empty());
    }

    #[test]
    fn scope_filters_by_path_prefix() {
        let now = Timestamp::now();
        let a = mk(Band::Live, "run:a", now, Some("/home/me/proj-a"));
        let b = mk(Band::Live, "run:b", now, Some("/home/me/proj-b"));
        let r = assemble(vec![a, b], vec![], Some(Path::new("/home/me/proj-a")), now, now);
        assert_eq!(r.live.len(), 1);
        assert_eq!(r.live[0].key, "run:a");
    }

    #[test]
    fn bands_sorted_newest_first_within() {
        let now = Timestamp::now();
        let older = now - std::time::Duration::from_secs(100);
        let r = assemble(
            vec![mk(Band::Happened, "old", older, None), mk(Band::Happened, "new", now, None)],
            vec![], None, older - std::time::Duration::from_secs(10), now,
        );
        assert_eq!(r.happened[0].key, "new", "newest first");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-daemon report::tests::since_bounds_only_happened`
Expected: FAIL — `assemble` / `Report` not defined.

- [ ] **Step 3: Write minimal implementation**

In `code/rl-proto/src/methods.rs`, beside the other method markers:

```rust
pub struct ReportGet;
impl Method for ReportGet {
    const NAME: &'static str = "report.get";
    type Params = ReportParams;
    type Result = Report;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<crate::Timestamp>,
}

/// The report: leads in three bands. needs-you is pinned; happened is bounded by `since`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub needs_you: Vec<crate::lead::Lead>,
    pub live: Vec<crate::lead::Lead>,
    pub happened: Vec<crate::lead::Lead>,
}
```

Bump `PROTOCOL_VERSION` at `code/rl-proto/src/lib.rs:18` (increment by one).

In `code/rl-daemon/src/report.rs`:

```rust
use rl_proto::methods::Report;
use std::path::Path;

/// Merge built-in and stored leads into the three bands. needs-you and live are
/// shown whatever their age; happened is bounded by `since`. Anything expired or
/// out of scope is dropped. Each band is newest-first.
pub fn assemble(
    builtins: Vec<Lead>,
    stored: Vec<Lead>,
    scope: Option<&Path>,
    since: Timestamp,
    now: Timestamp,
) -> Report {
    let mut report = Report::default();
    for lead in builtins.into_iter().chain(stored) {
        if lead.expires_at.is_some_and(|e| e <= now) {
            continue;
        }
        if let Some(s) = scope {
            match &lead.scope {
                Some(p) if p.starts_with(s) => {}
                _ => continue,
            }
        }
        match lead.band {
            Band::NeedsYou => report.needs_you.push(lead),
            Band::Live => report.live.push(lead),
            Band::Happened if lead.at >= since => report.happened.push(lead),
            Band::Happened => {}
        }
    }
    for band in [&mut report.needs_you, &mut report.live, &mut report.happened] {
        band.sort_by(|a, b| b.at.cmp(&a.at));
    }
    report
}
```

In `code/rl-daemon/src/handlers.rs`, inside `pub fn register(r: &mut Router<Daemon>)`, beside `DigestGet` (handlers.rs:419), register the read. Gather exactly as `DigestGet` does (it already assembles `firings` — reuse that gather), using `call_read`:

```rust
r.register::<ReportGet, _, _>(|d, p: ReportParams| async move {
    let now = Timestamp::now();
    // Default the happened window to the last 24h when no `since` is given.
    let since = p.since.unwrap_or_else(|| now - std::time::Duration::from_secs(24 * 3600));
    let scope = p.scope.clone();
    let report = d
        .ledger
        .call_read(move |c| {
            let decisions = rl_ledger::decisions::list(c, &DecisionFilter::default())?; // open-only by default
            let runs = rl_ledger::runs::list_for_digest(c, since)?;
            let firings = /* same firings gather the DigestGet handler uses at handlers.rs:419-437 */;
            let stored = rl_ledger::leads::list(c, now)?;
            let builtins = crate::report::project_builtins(&decisions, &runs, &firings);
            Ok(crate::report::assemble(builtins, stored, scope.as_deref(), since, now))
        })
        .await
        .map_err(RpcError::internal)?;
    Ok(report)
});
```

> Implementer note: copy the firings-gather line verbatim from the `DigestGet` closure (handlers.rs:419-437) so the firing list is sourced identically. If `DigestFilter`/`list_for_digest` needs a different arg than a bare `since`, match the DigestGet call. `DecisionFilter::default()` lists open decisions only (its `all` flag defaults false — confirm at `rl-proto` methods / `decisions::list` at decisions.rs:104). Import `ReportGet`, `ReportParams`, `DecisionFilter`, `Timestamp` at the top of handlers.rs.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rl-daemon report::tests` then `cargo build -p rl-daemon`
Expected: assemble tests PASS; daemon builds with the handler.

- [ ] **Step 5: Commit**

```bash
git add code/rl-daemon/src/report.rs code/rl-proto/src/methods.rs code/rl-proto/src/lib.rs code/rl-daemon/src/handlers.rs
git commit -m "The report: assemble bands and the report.get RPC; bump protocol"
```

---

## Task 5: `lead.post` / `lead.clear` RPC + handlers

**Files:**
- Modify: `code/rl-proto/src/methods.rs` (add `LeadPost`, `LeadPostParams`, `LeadClear`, `LeadClearParams`)
- Modify: `code/rl-daemon/src/handlers.rs` (register both, beside `EventEmit` at handlers.rs:513)
- Test: a handler-shape unit test is heavy; the behavior is covered by Task 2 (ledger) + Task 6 (CLI end-to-end). Add one proto serde test for the param structs.

**Interfaces:**
- Consumes: `rl_ledger::leads::{post, clear}` (Task 2); `rl_proto::lead::{Lead, Band, LeadStatus}`; `report::BUILTIN_SOURCE` (reserved-source guard).
- Produces: `LeadPost` (`lead.post`, Params `LeadPostParams`, Result `()`), `LeadClear` (`lead.clear`, Params `LeadClearParams`, Result `()`).
  - `LeadPostParams { source, key, band: String, title, summary: Option<String>, scope: Option<PathBuf>, action: Option<String>, ttl_secs: Option<u64> }`.
  - `LeadClearParams { source, key }`.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-proto/src/methods.rs  — in that file's #[cfg(test)] module (or add one)
#[test]
fn lead_params_round_trip() {
    let p = LeadPostParams {
        source: "feedback".into(), key: "triage".into(), band: "needs_you".into(),
        title: "5 to triage".into(), summary: None, scope: None,
        action: Some("claude -p 'triage'".into()), ttl_secs: Some(1800),
    };
    let s = serde_json::to_string(&p).unwrap();
    let back: LeadPostParams = serde_json::from_str(&s).unwrap();
    assert_eq!(p.source, back.source);
    assert_eq!(back.ttl_secs, Some(1800));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-proto lead_params_round_trip`
Expected: FAIL — `LeadPostParams` not defined.

- [ ] **Step 3: Write minimal implementation**

In `code/rl-proto/src/methods.rs`:

```rust
pub struct LeadPost;
impl Method for LeadPost {
    const NAME: &'static str = "lead.post";
    type Params = LeadPostParams;
    type Result = ();
}

pub struct LeadClear;
impl Method for LeadClear {
    const NAME: &'static str = "lead.clear";
    type Params = LeadClearParams;
    type Result = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadPostParams {
    pub source: String,
    pub key: String,
    pub band: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadClearParams {
    pub source: String,
    pub key: String,
}
```

In `code/rl-daemon/src/handlers.rs`, beside the `EventEmit` registration (handlers.rs:513):

```rust
r.register::<LeadPost, _, _>(|d, p: LeadPostParams| async move {
    if p.source == crate::report::BUILTIN_SOURCE || p.source.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "source {:?} is reserved; choose your own source name",
            crate::report::BUILTIN_SOURCE
        )));
    }
    let band: rl_proto::lead::Band = p.band.parse().map_err(RpcError::invalid_params)?;
    let now = Timestamp::now();
    let lead = rl_proto::lead::Lead {
        source: p.source,
        key: p.key,
        band,
        title: p.title,
        summary: p.summary,
        scope: p.scope,
        action: p.action,
        status: rl_proto::lead::LeadStatus::Open,
        at: now,
        expires_at: p.ttl_secs.map(|s| now + std::time::Duration::from_secs(s)),
    };
    d.ledger.call(move |c| rl_ledger::leads::post(c, &lead)).await.map_err(RpcError::internal)?;
    d.leads.wake.notify_one(); // nudge the sweeper (wired in Task 8)
    Ok(())
});

r.register::<LeadClear, _, _>(|d, p: LeadClearParams| async move {
    d.ledger
        .call(move |c| rl_ledger::leads::clear(c, &p.source, &p.key))
        .await
        .map_err(RpcError::internal)?;
    Ok(())
});
```

> Implementer note: `RpcError::invalid_params` — match the exact constructor the codebase uses for a bad-request error (grep `RpcError::` in handlers.rs; if there is only `RpcError::internal`, use the project's equivalent for a client error and follow an existing validating handler such as the `event.emit` kind check at handlers.rs:513-550). The `d.leads.wake.notify_one()` line depends on Task 8's `Daemon.leads` field — if implementing strictly in order, add that one line in Task 8 and leave it out here.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rl-proto lead_params_round_trip` then `cargo build -p rl-daemon`
Expected: PASS; daemon builds.

- [ ] **Step 5: Commit**

```bash
git add code/rl-proto/src/methods.rs code/rl-daemon/src/handlers.rs
git commit -m "The report: lead.post and lead.clear RPC, with the reserved-source guard"
```

---

## Task 6: `rl lead post` / `rl lead clear` CLI

**Files:**
- Modify: `code/rl-cli/src/main.rs` (add `Lead { cmd: LeadCmd }` to `enum Cmd` ~main.rs:57; `LeadCmd` + arg structs modeled on `WorkspaceCmd` ~main.rs:323 and `EmitArgs` ~main.rs:234; dispatch arm ~main.rs:490; handler)
- Test: a CLI integration test under `code/rl-cli/tests/` if the crate has them, else assert the parse wiring with a `clap` unit test.

**Interfaces:**
- Consumes: `daemon::ensure`, `Client::call`, `LeadPost`/`LeadPostParams`, `LeadClear`/`LeadClearParams`; `--in`/scope resolution as `emit` does (main.rs:1069-1107).
- Produces: the `rl lead post` / `rl lead clear` verbs.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-cli/src/main.rs  — in the #[cfg(test)] parse-tests module (mirror an existing clap test)
#[test]
fn lead_post_parses() {
    let cli = Cli::try_parse_from([
        "rl", "lead", "post", "--source", "feedback", "--key", "triage",
        "--band", "needs_you", "--title", "5 to triage", "--ttl", "1800",
    ])
    .unwrap();
    match cli.cmd {
        Some(Cmd::Lead { cmd: LeadCmd::Post(a) }) => {
            assert_eq!(a.source, "feedback");
            assert_eq!(a.band, "needs_you");
            assert_eq!(a.ttl_secs, Some(1800));
        }
        _ => panic!("expected lead post"),
    }
}

#[test]
fn lead_clear_parses() {
    let cli = Cli::try_parse_from(["rl", "lead", "clear", "--source", "feedback", "--key", "triage"]).unwrap();
    assert!(matches!(cli.cmd, Some(Cmd::Lead { cmd: LeadCmd::Clear(_) })));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-cli lead_post_parses`
Expected: FAIL — `Cmd::Lead` / `LeadCmd` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `enum Cmd` (main.rs:57):

```rust
    /// Post or clear a lead on the report, from a flow or a script.
    Lead {
        #[command(subcommand)]
        cmd: LeadCmd,
    },
```

Add the subcommand + args (model on `WorkspaceCmd` at main.rs:323 and `EmitArgs` at main.rs:234):

```rust
#[derive(Subcommand)]
enum LeadCmd {
    /// Raise or refresh a lead; re-posting the same --source/--key refreshes it.
    Post(LeadPostArgs),
    /// Clear a lead you posted.
    Clear(LeadClearArgs),
}

#[derive(Args)]
struct LeadPostArgs {
    /// Your source name — the thing raising leads (e.g. `feedback`, `gh`). Not `relay`.
    #[arg(long)]
    source: String,
    /// This lead's stable id within the source (e.g. `pr:412`).
    #[arg(long)]
    key: String,
    /// needs_you | live | happened.
    #[arg(long)]
    band: String,
    #[arg(long)]
    title: String,
    #[arg(long)]
    summary: Option<String>,
    /// The folder this is about.
    #[arg(long = "in", value_name = "DIR")]
    in_: Option<PathBuf>,
    /// How to act on it — a command to run or a thing to open.
    #[arg(long)]
    action: Option<String>,
    /// Expire the lead after this many seconds if not refreshed or cleared.
    #[arg(long = "ttl", value_name = "SECS")]
    ttl_secs: Option<u64>,
}

#[derive(Args)]
struct LeadClearArgs {
    #[arg(long)]
    source: String,
    #[arg(long)]
    key: String,
}
```

Dispatch arm (main.rs:490 match):

```rust
        Cmd::Lead { cmd } => lead(&home, cmd, cli.json).await,
```

Handler (model on `emit` at main.rs:1069 for scope resolution):

```rust
async fn lead(home: &Home, cmd: LeadCmd, json: bool) -> Result<()> {
    let mut client = daemon::ensure(home, !json).await?;
    match cmd {
        LeadCmd::Post(a) => {
            let scope = a.in_.map(|p| p.canonicalize().unwrap_or(p));
            let params = LeadPostParams {
                source: a.source,
                key: a.key,
                band: a.band,
                title: a.title,
                summary: a.summary,
                scope,
                action: a.action,
                ttl_secs: a.ttl_secs,
            };
            client.call::<LeadPost>(params).await?;
        }
        LeadCmd::Clear(a) => {
            client.call::<LeadClear>(LeadClearParams { source: a.source, key: a.key }).await?;
        }
    }
    Ok(())
}
```

Import `LeadPost, LeadPostParams, LeadClear, LeadClearParams` at the top of main.rs with the other `rl_proto::methods::*` uses.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rl-cli lead_post_parses lead_clear_parses` then `cargo build -p rl-cli`
Expected: PASS; CLI builds.

- [ ] **Step 5: Commit**

```bash
git add code/rl-cli/src/main.rs
git commit -m "The report: rl lead post and rl lead clear"
```

---

## Task 7: `rl report [scope] [since]` CLI + text rendering

**Files:**
- Modify: `code/rl-cli/src/main.rs` (`Report(ReportArgs)` variant + dispatch + handler)
- Modify: `code/rl-cli/src/render.rs` (a `lead_rows` builder + `LEAD_HEADERS`)
- Test: a `render` unit test on `lead_rows`; a clap parse test for `rl report`.

**Interfaces:**
- Consumes: `ReportGet`/`ReportParams`/`Report`; `render::table` (render.rs:65), `render::fmt_clock`/`state_label` patterns (render.rs:10,53); `parse_since` (used by `events`, main.rs:973).
- Produces: `rl report [SCOPE] [--since …]` printing three band sections; `render::lead_rows`.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-cli/src/render.rs  — #[cfg(test)]
#[test]
fn lead_rows_show_source_and_title() {
    use rl_proto::lead::{Band, Lead, LeadStatus};
    use rl_proto::Timestamp;
    let now = Timestamp::now();
    let leads = vec![Lead {
        source: "mobile developer".into(), key: "pr:412".into(), band: Band::NeedsYou,
        title: "review requested on #412".into(), summary: None, scope: None,
        action: Some("rl decide d1".into()), status: LeadStatus::Open, at: now, expires_at: None,
    }];
    let rows = lead_rows(&leads, now);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].iter().any(|c| c.contains("mobile developer")));
    assert!(rows[0].iter().any(|c| c.contains("review requested on #412")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-cli lead_rows_show_source_and_title`
Expected: FAIL — `lead_rows` not defined.

- [ ] **Step 3: Write minimal implementation**

In `code/rl-cli/src/render.rs` (mirror `event_rows`/`EVENT_HEADERS` at render.rs:35-50):

```rust
pub const LEAD_HEADERS: &[&str] = &["source", "when", "lead", "action"];

pub fn lead_rows(leads: &[rl_proto::lead::Lead], now: rl_proto::Timestamp) -> Vec<Vec<String>> {
    leads
        .iter()
        .map(|l| {
            vec![
                l.source.clone(),
                fmt_clock(l.at, now),
                match &l.summary {
                    Some(s) => format!("{} — {s}", l.title),
                    None => l.title.clone(),
                },
                l.action.clone().unwrap_or_default(),
            ]
        })
        .collect()
}
```

In `code/rl-cli/src/main.rs`, the `Report` variant on `enum Cmd`:

```rust
    /// What happened, what's live, and what needs you — the report.
    Report(ReportArgs),
```

```rust
#[derive(Args)]
struct ReportArgs {
    /// Limit to a folder (and its children).
    scope: Option<PathBuf>,
    /// How far back the `happened` band reaches (e.g. `2h`, `since yesterday`). Default 24h.
    #[arg(long)]
    since: Option<String>,
}
```

Dispatch arm:

```rust
        Cmd::Report(a) => report(&home, a, cli.json).await,
```

Handler:

```rust
async fn report(home: &Home, a: ReportArgs, json: bool) -> Result<()> {
    let now = now();
    let since = a.since.as_deref().map(|s| parse_since(s, now)).transpose()?;
    let scope = a.scope.map(|p| p.canonicalize().unwrap_or(p));
    let rep = daemon::ensure(home, !json).await?.call::<ReportGet>(ReportParams { scope, since }).await?;
    if json {
        println!("{}", serde_json::to_string(&rep)?);
        return Ok(());
    }
    let mut printed = false;
    for (heading, leads) in [("needs you", &rep.needs_you), ("live", &rep.live), ("happened", &rep.happened)] {
        if leads.is_empty() {
            continue;
        }
        if printed {
            println!();
        }
        println!("{heading}");
        print!("{}", render::table(render::LEAD_HEADERS, &render::lead_rows(leads, now)));
        printed = true;
    }
    if !printed {
        println!("nothing to report");
    }
    Ok(())
}
```

Import `ReportGet, ReportParams` at the top of main.rs. (`now()` and `parse_since` already exist — used by `events`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rl-cli lead_rows_show_source_and_title` then `cargo build -p rl-cli`
Expected: PASS; CLI builds.

- [ ] **Step 5: Commit**

```bash
git add code/rl-cli/src/main.rs code/rl-cli/src/render.rs
git commit -m "The report: rl report [scope] [since] as text"
```

---

## Task 8: The `leads` daemon service (sweep + push seat) + wiring

**Files:**
- Create: `code/rl-daemon/src/leads.rs`
- Modify: `code/rl-daemon/src/lib.rs` (`mod leads;`)
- Modify: `code/rl-daemon/src/server.rs` (build + spawn beside `flows`, server.rs:129-184; add `leads` field to `Daemon`, server.rs:26-42)
- Modify: `code/rl-daemon/src/handlers.rs` (the `d.leads.wake.notify_one()` line from Task 5 now compiles)
- Test: the sweep logic is tested at the ledger layer (Task 2). Add one test that `Leads::new` + an immediate sweep over a seeded in-memory ledger drops an expired lead (exercises the service calling `sweep_expired`).

**Interfaces:**
- Consumes: `rl_ledger::Ledger`, `rl_ledger::leads::sweep_expired`, `tokio_util::sync::CancellationToken`, `tokio::sync::Notify`, `rl_proto::Timestamp`.
- Produces: `crate::leads::Leads` (`Arc`-held service), `Leads::new(ledger, shutdown) -> Arc<Self>`, `Leads::serve(self: Arc<Self>)`, public `wake: Notify`. `Daemon.leads: Arc<crate::leads::Leads>`.

- [ ] **Step 1: Write the failing test**

```rust
// code/rl-daemon/src/leads.rs  (bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use rl_ledger::Ledger;
    use rl_proto::lead::{Band, Lead, LeadStatus};
    use rl_proto::Timestamp;

    #[tokio::test]
    async fn sweep_once_drops_expired() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        let mut l = Lead {
            source: "cron".into(), key: "n".into(), band: Band::Happened, title: "t".into(),
            summary: None, scope: None, action: None, status: LeadStatus::Open,
            at: now, expires_at: Some(now - std::time::Duration::from_secs(1)),
        };
        ledger.call(move |c| rl_ledger::leads::post(c, &l)).await.unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let svc = Leads::new(ledger.clone(), token.clone());
        svc.sweep().await.unwrap();
        let left = ledger.call(move |c| rl_ledger::leads::list(c, now)).await.unwrap();
        assert!(left.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rl-daemon leads::tests::sweep_once_drops_expired`
Expected: FAIL — `Leads` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// code/rl-daemon/src/leads.rs  (top)
//! The leads service: sweeps expired user leads, and is the seat the push hook
//! (step 4) will sit in. Correctness does not depend on it — the report read
//! path filters expiry itself; this is housekeeping.

use anyhow::Result;
use rl_ledger::Ledger;
use rl_proto::Timestamp;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How often the sweeper wakes on its own when nothing nudges it.
const TICK: Duration = Duration::from_secs(60);

pub struct Leads {
    ledger: Ledger,
    /// Rung when a lead is posted, so the loop looks again.
    pub wake: Notify,
    shutdown: CancellationToken,
}

impl Leads {
    pub fn new(ledger: Ledger, shutdown: CancellationToken) -> Arc<Self> {
        Arc::new(Self { ledger, wake: Notify::new(), shutdown })
    }

    /// Delete expired leads once.
    pub async fn sweep(&self) -> Result<()> {
        let now = Timestamp::now();
        self.ledger.call(move |c| rl_ledger::leads::sweep_expired(c, now)).await?;
        Ok(())
    }

    pub async fn serve(self: Arc<Self>) {
        loop {
            if let Err(e) = self.sweep().await {
                tracing::warn!(error = %format!("{e:#}"), "leads: sweep");
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }
}
```

Add `mod leads;` to `code/rl-daemon/src/lib.rs`.

In `code/rl-daemon/src/server.rs`, add the field to the `Daemon` struct (server.rs:26-42):

```rust
    pub leads: Arc<crate::leads::Leads>,
```

In `serve_with`, beside the `flows` spawn (after `tokio::spawn(flows.clone().serve());`, ~server.rs:170):

```rust
    let leads = crate::leads::Leads::new(ledger.clone(), shutdown.child_token());
    tokio::spawn(leads.clone().serve());
```

…and set `leads,` in the `Daemon { … }` initializer (server.rs:172-184). Note `ledger` is moved into `Daemon` at the end — clone for `Leads::new` before that move, as `flows`/`outside` do.

The `d.leads.wake.notify_one()` line added in Task 5's `LeadPost` handler now compiles.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rl-daemon leads::tests` then `cargo build -p rl-daemon`
Expected: PASS; daemon builds with the service wired.

- [ ] **Step 5: Commit**

```bash
git add code/rl-daemon/src/leads.rs code/rl-daemon/src/lib.rs code/rl-daemon/src/server.rs code/rl-daemon/src/handlers.rs
git commit -m "The report: the leads sweeper service, wired into serve_with"
```

---

## Task 9: Whole-crate gates + an end-to-end smoke test

**Files:**
- Test: `code/rl-cli/tests/report.rs` (new) if the CLI has an integration-test harness that spins a daemon; otherwise a daemon-level test exercising `lead.post` → `report.get`.

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the failing end-to-end test**

Find the existing integration-test pattern (grep `tests/` in `code/rl-cli` and `code/rl-daemon` for a test that starts a daemon / opens a `Ledger` and calls handlers). Mirror it. The test: post a user lead, project no built-ins, assemble, and assert it shows under its band.

```rust
// shape — adapt to the repo's harness
#[tokio::test]
async fn posted_lead_appears_in_report() {
    let ledger = rl_ledger::Ledger::open_in_memory().unwrap();
    let now = rl_proto::Timestamp::now();
    let lead = /* a needs_you user lead via rl_ledger::leads::post */;
    ledger.call(move |c| rl_ledger::leads::post(c, &lead)).await.unwrap();
    let stored = ledger.call(move |c| rl_ledger::leads::list(c, now)).await.unwrap();
    let report = rl_daemon::report::assemble(vec![], stored, None, now - std::time::Duration::from_secs(3600), now);
    assert_eq!(report.needs_you.len(), 1);
}
```

(If `rl_daemon::report` is not `pub`, expose it `pub mod report;` in lib.rs, or place the test inside the crate.)

- [ ] **Step 2: Run it, watch it fail, implement/expose as needed, watch it pass.**

- [ ] **Step 3: Run the full gates**

Run, in order, and fix anything:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean. A clippy warning is a CI failure — fix, don't allow.

- [ ] **Step 4: Manual smoke (optional but recommended)**

```bash
cargo run -p rl-cli -- lead post --source feedback --key triage --band needs_you --title "5 to triage" --action "echo triage"
cargo run -p rl-cli -- report
cargo run -p rl-cli -- lead clear --source feedback --key triage
cargo run -p rl-cli -- report
```
Expected: the lead shows under `needs you`, then is gone. (Kill any stale `rld` first so the new protocol takes — no backward compat.)

- [ ] **Step 5: Commit + open the PR**

```bash
git add -A
git commit -m "The report: end-to-end smoke test across lead.post and report.get"
```
Open one PR off `main` titled for report step 1, body linking the spec and this plan.

---

## Self-review notes

- **Spec coverage:** lead model → Task 1; source-owned keyed lifecycle (re-emit refreshes, clear, TTL) → Task 2; built-in projection (open decision → needs-you + `rl decide`; needs-you/live/happened runs; firing live/happened) → Task 3; `rl lead post`/`clear` as the `rl emit` extension → Tasks 5–6; `rl report [scope] [since]` as text → Tasks 4, 7; the `serve_with` background task as the push seat → Task 8. The co-worker layer and the pane are **out of step 1 by design** (separate roadmap items) — `rl report` groups by band only. Push delivery is step 4.
- **Deferred/decided-here:** source identity for built-ins is fixed as `source = "relay"`, `key = decision:/run:/firing:<id>` (the spec's open question, resolved minimally for step 1; co-worker tagging refines grouping later, not the key). Lead store = a dedicated `leads` table keyed `(source, key)`; built-ins stay computed (the spec's preferred option). Expiry is enforced at read time so correctness never depends on the sweeper.
- **Open question left for step 2+:** act-on-a-lead / focus-the-pane (`Lead.action` is carried but only printed in step 1).

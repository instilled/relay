# The report, step 2: the pane — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the report into the TUI as a full-width work-and-teams board — co-worker/team lanes with work/co-worker/when columns and a per-row band glyph — backed by a user-side source→tag store, dismissal, and act-on-a-lead.

**Architecture:** A new `source_tags` ledger table + `tag.set`/`tag.clear` RPC form the tag overlay (mirroring the `Order`/`hidden` pattern). Built-in leads gain a stable co-worker `source`. The daemon's `assemble` gains a dismissed-status filter and `post` gains a don't-resurrect guard; `ReportGet` carries the tag overlay inline on `Report`. A pure `lanes()` function groups/orders the report into lanes client-side; a new `View::Report` subpage renders it with the existing row/width machinery and wires act/tag/dismiss keys.

**Tech Stack:** Rust workspace; `rusqlite` ledger with numbered SQL migrations; `jiff::Timestamp`; serde JSON-RPC over a Unix socket; `ratatui` TUI.

**Spec:** `docs/specs/2026-10-20-the-report-step-2-pane.md` (and the arc spec `docs/specs/2026-10-17-the-report.md`). The plan argues from these; executors read both.

## Global Constraints

- **No backward compatibility.** Protocols are strict; bump the RPC `PROTOCOL_VERSION` (currently **6** after step 1) to **7** in `rl-proto`. Running `rld`/`rl` are killed and rebuilt on upgrade — no fallbacks for old peers.
- **Verify against CI gates before any push**, run verbatim, workspace-wide: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings` (a warning is a hard error); `cargo test --workspace`.
- **Migrations are append-only.** Latest on `main` is `0026`; add `0027_source_tags.sql` and register one tuple in `rl-ledger/src/migrate.rs`. Never edit a shipped migration. Timestamps are epoch millis via `rl_core::time`.
- **`Timestamp` is `jiff::Timestamp`** everywhere; duration via `now.duration_since(t)` → `jiff::SignedDuration`. No `crate::Timestamp` re-export.
- **Serde conventions:** enums `#[serde(rename_all = "snake_case")]`; optional fields `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- **Build off `main`** in a single fresh worktree; one PR. The code lives on `main` (`b3be655`), NOT on `chore/roadmap`.
- **TDD**: every task writes the failing test first, watches it fail, then the minimal code. Unit tests are in-file `#[cfg(test)] mod tests`; daemon integration tests in `code/rl-daemon/tests/`; CLI integration tests in `code/rl-cli/tests/`.

## Review Focus

Each line is pinned to the task that owns its test.

- **No sources tagged yet (first run):** every lane resolves to a default (project basename, or *my work*) — never empty, never a path. → Task 6 (`lanes` default resolution).
- **A lead whose `scope` is `None`:** project column renders blank and the lane falls to *my work*; no `/` or panic. → Task 6.
- **A co-worker across two projects:** rows with differing project values stay under one lane, sorted, not implying one project. → Task 6.
- **80-column terminal:** lead column flexes, project/when elide first, no unaligned wrap. → Task 7 (`report_lines` width).
- **A dismissed lead whose source re-emits the same key:** stays dismissed (post guard + assemble filter). → Task 4.
- **Tagging a source with leads already in all three bands:** all move lanes at once (tag is on the source). → Task 6.

---

## Task 1: `source_tags` table + `tags` ledger module

**Files:**
- Create: `code/rl-ledger/migrations/0027_source_tags.sql`
- Modify: `code/rl-ledger/src/migrate.rs` (append one `MIGRATIONS` tuple)
- Create: `code/rl-ledger/src/tags.rs`
- Modify: `code/rl-ledger/src/lib.rs` (add `pub mod tags;`)

**Interfaces:**
- Produces: `rl_ledger::tags::{set, clear, list}`; `SourceTag { source: String, tag: String }` (defined in rl-proto, Task 2 — for Task 1 use a local tuple/struct and switch to the proto type in Task 2, OR define the proto type first; to avoid churn, **define `rl_proto::methods::SourceTag` as the very first step here** and depend on it).

- [ ] **Step 1: Define the shared row type** in `code/rl-proto/src/methods.rs` (near `Order`, methods.rs:659):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTag {
    pub source: String,
    pub tag: String,
}
```

- [ ] **Step 2: Write the migration** `0027_source_tags.sql`:

```sql
-- A user-side overlay: tag a lead source with a co-worker/team name.
-- Set once per source; every future lead from that source inherits it.
CREATE TABLE source_tags (
    source TEXT NOT NULL PRIMARY KEY,
    tag    TEXT NOT NULL
);
```

- [ ] **Step 3: Register it** in `migrate.rs` `MIGRATIONS` (append after the `0026_leads` tuple at migrate.rs:54):

```rust
(27, "0027_source_tags", include_str!("../migrations/0027_source_tags.sql")),
```

- [ ] **Step 4: Write the failing tests** in `code/rl-ledger/src/tags.rs` `#[cfg(test)] mod tests` (mirror `leads.rs:96` style — `Ledger::open_in_memory()` + `call_blocking`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ledger;

    #[test]
    fn set_then_list_roundtrips() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| { set(c, "claude", "dev")?; set(c, "codex", "dev")?; Ok(()) }).unwrap();
        let tags = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags.iter().any(|t| t.source == "claude" && t.tag == "dev"));
    }

    #[test]
    fn set_is_an_upsert() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| { set(c, "claude", "dev")?; set(c, "claude", "backend")?; Ok(()) }).unwrap();
        let tags = led.call_blocking(|c| list(c)).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag, "backend");
    }

    #[test]
    fn clear_removes_and_is_idempotent() {
        let led = Ledger::open_in_memory().unwrap();
        led.call_blocking(|c| { set(c, "claude", "dev")?; clear(c, "claude")?; clear(c, "claude")?; Ok(()) }).unwrap();
        assert!(led.call_blocking(|c| list(c)).unwrap().is_empty());
    }
}
```

- [ ] **Step 5: Run the tests, watch them fail** (`set`/`list`/`clear` not defined): `cargo test -p rl-ledger tags::`.

- [ ] **Step 6: Implement** `code/rl-ledger/src/tags.rs` (model on `order.rs`/`hidden.rs`):

```rust
use crate::Result;
use rl_proto::methods::SourceTag;
use rusqlite::Connection;

pub fn set(conn: &Connection, source: &str, tag: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO source_tags (source, tag) VALUES (?1, ?2)
         ON CONFLICT(source) DO UPDATE SET tag = excluded.tag",
        (source, tag),
    )?;
    Ok(())
}

pub fn clear(conn: &Connection, source: &str) -> Result<()> {
    conn.execute("DELETE FROM source_tags WHERE source = ?1", [source])?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<SourceTag>> {
    let mut st = conn.prepare("SELECT source, tag FROM source_tags ORDER BY source")?;
    let rows = st
        .query_map([], |r| Ok(SourceTag { source: r.get(0)?, tag: r.get(1)? }))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

- [ ] **Step 7: Run the tests, watch them pass.** Then `cargo fmt --all` and `cargo clippy -p rl-ledger --all-targets -- -D warnings`.

- [ ] **Step 8: Commit** — `feat(ledger): source_tags table + tags set/clear/list (report step 2)`.

---

## Task 2: `tag.set` / `tag.clear` RPC + tags carried on `Report`

**Files:**
- Modify: `code/rl-proto/src/methods.rs` (methods + `Report.tags`; bump `PROTOCOL_VERSION`)
- Modify: `code/rl-daemon/src/handlers.rs` (register `TagSet`/`TagClear`; populate `Report.tags` in `ReportGet`)
- Create/Modify: `code/rl-daemon/tests/tags.rs` (integration)

**Interfaces:**
- Consumes: `rl_ledger::tags::{set, clear, list}` (Task 1); `SourceTag` (Task 1).
- Produces: `TagSet`/`TagClear` methods; `Report { needs_you, live, happened, tags: Vec<SourceTag> }`.

- [ ] **Step 1: Write the failing integration test** `code/rl-daemon/tests/tags.rs` (mirror `tests/report.rs`):

```rust
#[tokio::test]
async fn a_tagged_source_rides_on_the_report() {
    let led = rl_ledger::Ledger::open_in_memory().unwrap();
    led.call(|c| rl_ledger::tags::set(c, "claude", "dev")).await.unwrap();
    let tags = led.call(|c| rl_ledger::tags::list(c)).await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].tag, "dev");
}
```
(An end-to-end `TagSet`→`ReportGet` test is added once the `Daemon` test harness in `tests/common/` is in hand; the implementer uses the same harness `tests/report.rs` uses if it drives a live daemon, else the ledger-level assertion above plus the unit coverage in Task 1 suffice. Record which in the report.)

- [ ] **Step 2: Add `tags` to `Report`** (methods.rs:570):

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub needs_you: Vec<crate::lead::Lead>,
    pub live: Vec<crate::lead::Lead>,
    pub happened: Vec<crate::lead::Lead>,
    #[serde(default)]
    pub tags: Vec<SourceTag>,
}
```

- [ ] **Step 3: Declare the two methods** (methods.rs, near `LeadPost` :189):

```rust
pub struct TagSet;
impl Method for TagSet {
    const NAME: &'static str = "tag.set";
    type Params = SourceTag;        // { source, tag }
    type Result = ();
}
pub struct TagClear;
impl Method for TagClear {
    const NAME: &'static str = "tag.clear";
    type Params = TagClearParams;   // { source }
    type Result = ();
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagClearParams { pub source: String }
```

- [ ] **Step 4: Bump `PROTOCOL_VERSION`** wherever it is defined in `rl-proto` (grep `PROTOCOL_VERSION`): `6` → `7`.

- [ ] **Step 5: Register the handlers** in `handlers.rs` `register` (write path = `d.ledger.call`):

```rust
r.register::<TagSet, _, _>(|d, p: SourceTag| async move {
    if p.source.is_empty() || p.tag.is_empty() {
        return Err(RpcError::invalid_params("source and tag must be non-empty".to_string()));
    }
    d.ledger.call(move |c| rl_ledger::tags::set(c, &p.source, &p.tag)).await.map_err(RpcError::internal)
});
r.register::<TagClear, _, _>(|d, p: TagClearParams| async move {
    d.ledger.call(move |c| rl_ledger::tags::clear(c, &p.source)).await.map_err(RpcError::internal)
});
```

- [ ] **Step 6: Populate `Report.tags` in `ReportGet`** (handlers.rs:419) — add inside the `call_read` closure, after `assemble`:

```rust
let tags = rl_ledger::tags::list(c)?;
let mut report = crate::report::assemble(builtins, stored, scope.as_deref(), since, now);
report.tags = tags;
Ok(report)
```

- [ ] **Step 7: Run tests, fmt, clippy; watch pass.** `cargo test -p rl-daemon`.

- [ ] **Step 8: Commit** — `feat(rpc): tag.set/tag.clear + Report.tags; PROTOCOL_VERSION 7`.

---

## Task 3: Built-in leads carry a co-worker `source`

**Files:**
- Modify: `code/rl-daemon/src/report.rs` (`project_builtins` source derivation; existing tests)
- Modify: `code/rl-daemon/src/handlers.rs` (`LeadPost` reserved-source guard)

**Interfaces:**
- Consumes: `Run.cli: Option<String>`, `Run.kind: RunKind` (`Agent|Shell|Command|Plugin`), `Run.workspace`, `Decision.workspace`, firing→flow name.
- Produces: built-in `Lead.source` = the co-worker; a `MY_WORK` sentinel const; unchanged `Lead.scope` (= workspace path).

**Ruling (record in ledger):** *What you tag is the co-worker.* A built-in run lead's `source` becomes the co-worker — `r.cli` when `Some`, else a kind label (`shell`/`command`/`plugin`); a firing lead's `source` is its flow name; a decision lead's `source` is the `MY_WORK` sentinel in this cut (decisions are not a co-worker; revisit if dogfooding wants them under their raising agent). `scope` stays the workspace path, so the untagged default lane is still the project. *Why:* matches the work-and-teams mocks (tag claude+codex→dev) and keeps the project as a column, not the tag key. *Cost if wrong:* re-tagging semantics for decisions change; contained to `project_builtins` + its tests.

- [ ] **Step 1: Replace the `BUILTIN_SOURCE` reserved-name constant** in `report.rs:13` with the sentinel and a helper:

```rust
/// Built-in leads that belong to no co-worker sit here until tagged.
pub const MY_WORK: &str = "my work";

/// The co-worker a run lead belongs to: its detected CLI, else its kind.
fn run_source(r: &Run) -> String {
    r.cli.clone().unwrap_or_else(|| match r.kind {
        RunKind::Shell => "shell".into(),
        RunKind::Command => "command".into(),
        RunKind::Plugin => "plugin".into(),
        RunKind::Agent => "agent".into(),
    })
}
```

- [ ] **Step 2: Write the failing tests** in `report.rs` `mod tests` (extend the existing block at report.rs:133; reuse `test_run`/`test_decision`/`test_firing`). Note `test_run` may need a `cli`/`kind` setter — add a helper `test_run_cli(id, state, cli: Option<&str>, kind)` if the existing builder is fixed:

```rust
#[test]
fn run_lead_source_is_the_cli() {
    let mut r = test_run("r1", RunState::Working);
    r.cli = Some("claude".into());
    let leads = project_builtins(&[], &[r], &[], Timestamp::now());
    assert_eq!(leads[0].source, "claude");
}

#[test]
fn shell_run_without_cli_sources_to_shell() {
    let mut r = test_run("r1", RunState::Working);
    r.cli = None; r.kind = RunKind::Shell;
    let leads = project_builtins(&[], &[r], &[], Timestamp::now());
    assert_eq!(leads[0].source, "shell");
}

#[test]
fn decision_lead_sources_to_my_work() {
    let d = test_decision("d1");
    let leads = project_builtins(&[d], &[], &[], Timestamp::now());
    assert_eq!(leads[0].source, MY_WORK);
}
```

- [ ] **Step 3: Run, watch fail** (`source` still `"relay"`). `cargo test -p rl-daemon report::`.

- [ ] **Step 4: Implement** — in `project_builtins` replace `source: BUILTIN_SOURCE.into()` with: runs → `run_source(r)` (report.rs:63); decisions → `MY_WORK.into()` (report.rs:29); firings → the flow name of `f` (report.rs:82; derive from the firing — use the flow-name field on `Firing`; if only an id is present, fall to `MY_WORK` and note it).

- [ ] **Step 5: Relax the `LeadPost` reserved-source guard** (handlers.rs:535) — reject only empty and the `MY_WORK` sentinel:

```rust
if p.source.is_empty() || p.source == crate::report::MY_WORK {
    return Err(RpcError::invalid_params(
        "source must be non-empty and not \"my work\" (reserved)".to_string()));
}
```

- [ ] **Step 6: Fix any existing report tests** that asserted `source == "relay"`. Run `cargo test -p rl-daemon`, fmt, clippy.

- [ ] **Step 7: Commit** — `feat(report): built-in leads carry a co-worker source`.

---

## Task 4: Dismissal — `lead.dismiss`, the don't-resurrect guard, the status filter

**Files:**
- Modify: `code/rl-ledger/src/leads.rs` (`post` upsert guard; `dismiss` fn)
- Modify: `code/rl-proto/src/methods.rs` (`LeadDismiss` method)
- Modify: `code/rl-daemon/src/handlers.rs` (register `LeadDismiss`)
- Modify: `code/rl-daemon/src/report.rs` (`assemble` drops dismissed)

**Interfaces:**
- Produces: `rl_ledger::leads::dismiss(conn, source, key)`; `LeadDismiss` RPC; `assemble` filters `LeadStatus::Dismissed`.

- [ ] **Step 1: Write the failing ledger tests** in `leads.rs mod tests`:

```rust
#[test]
fn post_does_not_resurrect_a_dismissed_lead() {
    let led = Ledger::open_in_memory().unwrap();
    let now = jiff::Timestamp::now();
    let l = mk("feedback", "triage", Band::NeedsYou, now); // existing test builder
    led.call_blocking({ let l = l.clone(); move |c| post(c, &l) }).unwrap();
    led.call_blocking(|c| dismiss(c, "feedback", "triage")).unwrap();
    led.call_blocking(move |c| post(c, &l)).unwrap();           // source re-emits
    let got = led.call_blocking(move |c| list(c, now)).unwrap();
    assert_eq!(got[0].status, LeadStatus::Dismissed, "re-emit must not un-dismiss");
}
```

- [ ] **Step 2: Write the failing assemble test** in `report.rs mod tests`:

```rust
#[test]
fn assemble_drops_dismissed_leads() {
    let now = Timestamp::now();
    let mut l = mk("feedback", "triage", Band::NeedsYou, now); // existing report.rs builder
    l.status = LeadStatus::Dismissed;
    let report = assemble(vec![], vec![l], None, now - std::time::Duration::from_secs(3600), now);
    assert!(report.needs_you.is_empty());
}
```

- [ ] **Step 3: Run, watch both fail.**

- [ ] **Step 4: Implement the post guard** — in `leads.rs::post`, make the `ON CONFLICT` keep a dismissed status:

```sql
ON CONFLICT(source, key) DO UPDATE SET
    band = excluded.band, title = excluded.title, summary = excluded.summary,
    scope = excluded.scope, action = excluded.action, updated_at = excluded.updated_at,
    expires_at = excluded.expires_at,
    status = CASE WHEN leads.status = 'dismissed' THEN 'dismissed' ELSE excluded.status END
```

- [ ] **Step 5: Add `dismiss`** to `leads.rs`:

```rust
pub fn dismiss(conn: &Connection, source: &str, key: &str) -> Result<()> {
    conn.execute(
        "UPDATE leads SET status = 'dismissed' WHERE source = ?1 AND key = ?2",
        (source, key),
    )?;
    Ok(())
}
```

- [ ] **Step 6: Filter dismissed in `assemble`** — in the per-lead loop (report.rs:110), `continue` when `lead.status == LeadStatus::Dismissed`.

- [ ] **Step 7: Declare + register `LeadDismiss`** in `methods.rs` (near `LeadClear` :199) and `handlers.rs`:

```rust
// methods.rs
pub struct LeadDismiss;
impl Method for LeadDismiss {
    const NAME: &'static str = "lead.dismiss";
    type Params = LeadDismissParams;
    type Result = ();
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadDismissParams { pub source: String, pub key: String }

// handlers.rs — write path
r.register::<LeadDismiss, _, _>(|d, p: LeadDismissParams| async move {
    d.ledger.call(move |c| rl_ledger::leads::dismiss(c, &p.source, &p.key)).await.map_err(RpcError::internal)
});
```

- [ ] **Step 8: Run tests, fmt, clippy; commit** — `feat(leads): dismiss + don't-resurrect guard + assemble status filter`.

---

## Task 5: `rl tag` CLI

**Files:**
- Modify: `code/rl-cli/src/main.rs` (`Cmd::Tag`, `TagArgs`, dispatch, `tag()` fn)
- Create: `code/rl-cli/tests/tag.rs` (integration, mirror `tests/workspace.rs`)

**Interfaces:**
- Consumes: `TagSet`/`TagClear` RPC (Task 2); `SourceTag`/`TagClearParams`.

- [ ] **Step 1: Write the failing CLI integration test** `code/rl-cli/tests/tag.rs` (match the harness in `tests/workspace.rs` — spawn a daemon against a temp home, run the binary, assert). Assert `rl tag claude dev` succeeds and a subsequent `rl report --json` groups/returns the tag (or, if the harness only checks exit + output, assert the command exits 0 and `rl tag --clear claude` too).

- [ ] **Step 2: Add the `Cmd` variant + args** in `main.rs`:

```rust
/// Tag a lead source with a co-worker or team name.
Tag(TagArgs),            // in enum Cmd (main.rs:59)

#[derive(Args)]
struct TagArgs {
    /// The lead source (a co-worker, flow, or `rl lead --source` name).
    source: String,
    /// The co-worker/team name to file it under. Omit with --clear.
    name: Option<String>,
    /// Remove the tag on this source instead of setting one.
    #[arg(long)] clear: bool,
}
```

- [ ] **Step 3: Add the dispatch arm** (main.rs:541+): `Cmd::Tag(a) => tag(&home, a, cli.json).await,`.

- [ ] **Step 4: Implement `tag()`** (model on `lead()` main.rs:1201):

```rust
async fn tag(home: &Home, a: TagArgs, json: bool) -> Result<()> {
    let client = daemon::ensure(home, !json).await?;
    if a.clear {
        client.call::<TagClear>(TagClearParams { source: a.source }).await?;
    } else {
        let tag = a.name.ok_or_else(|| anyhow!("give a name, or pass --clear"))?;
        client.call::<TagSet>(SourceTag { source: a.source, tag }).await?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests, fmt, clippy; commit** — `feat(cli): rl tag <source> <name> / --clear`.

---

## Task 6: `lanes()` — pure grouping, lane resolution, and ordering

**Files:**
- Create: `code/rl-cli/src/tui/report.rs` (the `lanes` function + types; `pub mod report;` added to `tui/mod.rs`)

**Interfaces:**
- Consumes: `rl_proto::methods::{Report, SourceTag}`, `rl_proto::lead::{Lead, Band, LeadStatus}`.
- Produces:

```rust
pub struct Lane {
    pub name: String,       // the tag, or project basename, or "my work"
    pub is_team: bool,      // the tag groups >1 source
    pub needs: usize, pub live: usize, pub done: usize,
    pub rows: Vec<Row>,     // ordered needs → live → happened, newest-first within
}
pub struct Row<'a> { pub lead: &'a Lead }   // band read from lead.band
pub fn lanes<'a>(report: &'a Report) -> Vec<Lane /* borrowing report */>;
```
(Exact lifetime shape is the implementer's call — a `Lane` owning cloned `Lead`s is acceptable if borrowing fights the renderer. Record the choice.)

**Lane resolution (the core rule):** for each lead across all three bands, its lane name =
1. the `tag` for `lead.source` if the source is tagged (from `report.tags`); else
2. the project basename of `lead.scope` (`Path::file_name`) if `scope` is `Some` and non-empty; else
3. `"my work"`.
A lane `is_team` when more than one distinct `source` maps into it.

**Ordering:** lanes holding ≥1 needs-you lead sort before lanes without; ties broken by most-recent `at`. Within a lane, rows sort by band (`NeedsYou` < `Live` < `Happened`) then `at` descending.

- [ ] **Step 1: Write the failing unit tests** in `report.rs mod tests` (build `Report` values directly with `mk_lead(source, key, band, scope, at)`):

```rust
#[test]
fn untagged_lead_lands_under_its_project_basename() {
    let r = Report { happened: vec![], live: vec![mk_lead("shell", "r1", Band::Live, Some("/Users/x/Projects/relay"), ts(1))], needs_you: vec![], tags: vec![] };
    let ls = lanes(&r);
    assert_eq!(ls[0].name, "relay");
    assert!(!ls[0].is_team);
}

#[test]
fn tag_moves_every_band_of_a_source_into_one_lane() {
    let r = Report {
        needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(3))],
        live: vec![mk_lead("claude", "b", Band::Live, Some("/p/relay"), ts(2))],
        happened: vec![mk_lead("codex", "c", Band::Happened, Some("/p/relay"), ts(1))],
        tags: vec![SourceTag { source: "claude".into(), tag: "dev".into() },
                   SourceTag { source: "codex".into(),  tag: "dev".into() }],
    };
    let ls = lanes(&r);
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].name, "dev");
    assert!(ls[0].is_team);          // two sources
    assert_eq!(ls[0].rows.len(), 3);
    assert_eq!(ls[0].rows[0].lead.band, Band::NeedsYou); // needs pinned to top
}

#[test]
fn needs_bearing_lanes_sort_first() {
    let r = Report {
        needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(1))],
        live: vec![mk_lead("shell", "b", Band::Live, Some("/p/ops"), ts(9))],
        happened: vec![], tags: vec![],
    };
    let ls = lanes(&r);
    assert_eq!(ls[0].name, "relay");  // has needs-you, beats the newer live-only "ops"
}

#[test]
fn scopeless_lead_falls_to_my_work() {
    let r = Report { needs_you: vec![], live: vec![], tags: vec![],
        happened: vec![mk_lead("agent", "x", Band::Happened, None, ts(1))] };
    assert_eq!(lanes(&r)[0].name, "my work");
}
```

- [ ] **Step 2: Run, watch fail. Step 3: Implement `lanes()`** per the rules above (a `HashMap<String, Lane>` keyed by resolved lane name, tracking a `HashSet<source>` per lane for `is_team`, then sort). **Step 4: Run, watch pass.** fmt, clippy. **Step 5: Commit** — `feat(tui): lanes() — group the report into work-and-team lanes`.

---

## Task 7: `View::Report` — render the board

**Files:**
- Modify: `code/rl-cli/src/tui/report.rs` (add `report_lines`)
- Modify: `code/rl-cli/src/tui/mod.rs` (`View::Report`; `report`/`report_tags` fields; draw dispatch; `draw_subpage_body` branch; fetch)
- Modify: `code/rl-cli/src/tui/palette.rs` (a `report` command) and `code/rl-cli/src/tui/keys.rs` (dispatch)
- Modify: `code/rl-cli/src/tui/feed_ops.rs` or a new `open_report` (mirror `toggle_events`/`open_catch_up`)

**Interfaces:**
- Consumes: `lanes()` (Task 6); `fit`/`place_parts` (`rows::{fit, place_parts}`), `wrap`/`cap_spans`; `self.rpc.call::<ReportGet>`.
- Produces: `report_lines(report: &Report, width: usize, now: Timestamp) -> Vec<Line<'static>>`.

- [ ] **Step 1: Write a failing unit test** for `report_lines` width behaviour (assert the produced line strings, via a `Line::to_string()` helper or by inspecting spans): at width 80 a long title is truncated with `…` and the lane header line contains the tally; at width 40 the `when` column is dropped before the lead text becomes empty. Keep assertions on substrings, not exact layout.

- [ ] **Step 2: Implement `report_lines`** — call `lanes(report)`; for each lane emit a header `Line` (name, a `team` marker when `is_team`, the `N needs · N live · N done` tally) then a dim `─` rule, then one `Line` per row built from columns via `fit()`: state glyph (`⚠`/`•`/`✓`, needs styled with `theme::needs()`), lead (flex = `width - fixed`), project basename, co-worker (`row.lead.source` + kind glyph), `when` (`fmt_clock`). Budget right-hand columns first; drop `when` then `project` as width shrinks (floor the lead column at ~20, like `pane.rs`). No borders.

- [ ] **Step 3: Add the `View::Report` variant** (mod.rs:146) and App fields: `report: Option<Report>` and the view's selection. In `draw` (mod.rs:2098) add `View::Report => self.draw_report(f, list),` or render through `draw_subpage_body`; follow whichever path `draw_catch_up` uses. Add the `draw_subpage_body` branch (mod.rs:2741) when `self.view == View::Report`:

```rust
if self.view == View::Report {
    let lines = match &self.report {
        Some(rep) => report::report_lines(rep, area.width as usize, Timestamp::now()),
        None => vec![Line::from(Span::raw(" loading report…").dim())],
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    return;
}
```

- [ ] **Step 4: Fetch on open** — add `open_report` (mirror `open_catch_up` catch_ops.rs:13): `self.open_view(View::Report); self.leave_subpage();` then `self.report = Some(self.rpc.call::<ReportGet>(ReportParams::default()).await?);`. Also refresh it on the panel's existing event tick in `refresh` (mod.rs:1531) when `self.view == View::Report`.

- [ ] **Step 5: Add the palette command** (palette.rs:57 `ALL`, copy the `events` entry at :142) named `report`, and a `palette::Cmd::Report` arm in `keys.rs` `command` (keys.rs:1033) → `self.open_report().await`. Pick a free hard-key if one is wanted (record the choice).

- [ ] **Step 6: Add an App-level test** (inline in `tui/mod.rs`, mirror the `open_catch_up` tests ~mod.rs:4860) that seeds `self.report` and asserts `open_report`/draw produce lane headers. **Step 7: Run tests, fmt, clippy; commit** — `feat(tui): View::Report renders the work-and-teams board`.

---

## Task 8: Act on a lead, tag, and dismiss from the pane

**Files:**
- Modify: `code/rl-cli/src/tui/keys.rs` (`key_subpage` for `View::Report`: enter/t/d)
- Modify: `code/rl-cli/src/tui/report.rs` or `act.rs` (the action handlers)
- Modify: `code/rl-cli/src/tui/mod.rs` (`Mode` for the tag prompt if needed; selection in the report view)

**Interfaces:**
- Consumes: `TagSet`/`TagClear`, `LeadDismiss` RPC; the selected row's `lead` (source/key, and the underlying run/decision id parsed from `lead.key` — `run:<id>`/`decision:<id>`).

- [ ] **Step 1: Write failing tests** (inline App tests): with a report loaded and a row selected, (a) `dismiss_selected()` issues `LeadDismiss` with the row's `source`/`key` (assert via a fake/record client or the real in-memory daemon harness if the TUI tests use one); (b) `tag_selected("dev")` issues `TagSet { source, "dev" }`; (c) acting on a `run:<id>` lead focuses/opens that run. Use whatever client-observation seam the existing `act.rs` tests use; if none, assert the state transition (`Mode`) and leave the RPC to a thin wrapper covered by Task 2/4.

- [ ] **Step 2: Add selection + keys for `View::Report`** in `key_subpage` (keys.rs:388): a row cursor (up/down), `enter` → act, `t` → tag prompt (a `Mode::Tag`-style input echoing the existing rename prompt), `d` → dismiss.

- [ ] **Step 3: Implement the handlers:**
  - `dismiss_selected`: `self.rpc.call::<LeadDismiss>(LeadDismissParams { source, key }).await?;` then re-fetch the report.
  - `tag_selected(name)`: `self.rpc.call::<TagSet>(SourceTag { source, tag: name }).await?;` re-fetch.
  - `act_selected`: parse `lead.key` — `run:<id>` → open that run in the panel (`switch_to(View::Work)` + select the run, the same path a list selection uses); `decision:<id>` → open the decision/answer flow; a user lead with an `action` → run it via the same path `rl lead` actions use. Set the run built-in lead's `action` here only if acting needs it; otherwise key-parse is enough.

- [ ] **Step 4: Run tests, fmt, clippy; commit** — `feat(tui): act on / tag / dismiss a lead from the report pane`.

---

## Final review

After Task 8: dispatch the whole-branch review on the most capable model, run the three CI gates one last time, then `superpowers:finishing-a-development-branch` → **Push + open PR off `main`** (the integration/merge decision is the user's).

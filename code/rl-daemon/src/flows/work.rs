//! A firing's folder, `runs/<firing>/` in its flow's: `$RELAY_WORK`, the files its steps share.
//! Under `.relay/<step>/`, apart from them, what Relay hands a step — `in/<step>.out` for each
//! step it comes after, `$RELAY_IN` — and what the step hands back: `out`, `$RELAY_OUT`, its
//! `key=value` lines read when it ends. A name from a dot is never a step's file
//! (`rl_flow::state::file_name`), so the two never meet.

use super::Flows;
use super::engine::def_of;
use anyhow::{Context, Result};
use rl_core::Home;
use rl_flow::advance::Happened;
use rl_flow::state::{OUT_FILE_MAX, out_lines};
use rl_ledger::firings;
use rl_proto::flow::StepStatus;
use rl_proto::types::event_kind;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::warn;

/// Relay's own corner of a firing's folder.
const OWN: &str = ".relay";

/// Where one step of one firing finds and leaves things.
pub(super) struct Work {
    /// `$RELAY_WORK`: the firing's folder.
    pub(super) dir: PathBuf,
    /// `$RELAY_IN`: what the steps before it printed, `<step>.out` each.
    pub(super) input: PathBuf,
    /// `$RELAY_OUT`: `key=value` lines the step appends.
    pub(super) out: PathBuf,
}

impl Work {
    pub(super) fn of(home: &Home, flow: &str, firing: &str, step: &str) -> Self {
        let dir = home.firing_dir(flow, firing);
        let own = dir.join(OWN).join(step);
        Self {
            input: own.join("in"),
            out: own.join("out"),
            dir,
        }
    }

    /// Made ready for the step to start: its folders there, what each step before it printed
    /// written under `$RELAY_IN`, and `$RELAY_OUT` there to append to. What the step appended
    /// before — a lost run's, picked up again — is kept.
    pub(super) fn prepare(&self, outs: &[(String, String)]) -> Result<()> {
        std::fs::create_dir_all(&self.input).with_context(|| format!("making {}", self.input.display()))?;
        for (step, out) in outs {
            write_atomic(&self.input.join(format!("{step}.out")), out.as_bytes())?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.out)
            .with_context(|| format!("making {}", self.out.display()))?;
        Ok(())
    }

    /// What the step appended to `$RELAY_OUT`, whole; `None` when there is no such file.
    /// Refused, saying why, past 1 MB or when it is not UTF-8.
    pub(super) fn appended(&self) -> Result<Option<String>, String> {
        let f = match std::fs::File::open(&self.out) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("$RELAY_OUT could not be read: {e}")),
        };
        let mut bytes = Vec::new();
        f.take(OUT_FILE_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("$RELAY_OUT could not be read: {e}"))?;
        if bytes.len() > OUT_FILE_MAX {
            return Err("$RELAY_OUT is over 1 MB".to_string());
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| "$RELAY_OUT is not UTF-8".to_string())
    }
}

/// `bytes` at `path` all at once: written beside it under a dot name, then renamed over it, so a
/// reader sees the old file or the new, never part of one.
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().context("a file with no folder")?;
    let name = path.file_name().context("a file with no name")?.to_string_lossy();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

impl Flows {
    /// A step's run ended well: what it appended to `$RELAY_OUT` becomes its values, before
    /// anything after it is decided. What cannot be kept is said in `flow.refused` and the step
    /// fails — a step that meant to say something does not let the rest go on without it. Only a
    /// step still running is read, so an end seen twice is read once.
    pub(super) async fn kept(&self, firing: &str, h: Happened) -> Happened {
        let Happened::Ended { step, ok: true } = &h else {
            return h;
        };
        let step = step.clone();
        let (id, s) = (firing.to_string(), step.clone());
        let found = self
            .ledger
            .call(move |c| {
                let running = firings::steps(c, &id)?.get(&s) == Some(&StepStatus::Running);
                if !running {
                    return Ok(None);
                }
                def_of(c, &id)
            })
            .await;
        let def = match found {
            Ok(Some(def)) => def,
            Ok(None) => return h,
            Err(e) => {
                warn!(firing, step, error = %format!("{e:#}"), "reading a step's $RELAY_OUT");
                return h;
            }
        };
        let work = Work::of(&self.home, &def.name, firing, &step);
        let why = match work.appended().and_then(|t| t.as_deref().map(out_lines).transpose()) {
            Ok(None) => return h,
            Ok(Some(values)) if values.is_empty() => return h,
            Ok(Some(values)) => {
                let (id, s) = (firing.to_string(), step.clone());
                match self
                    .ledger
                    .call(move |c| firings::set_values(c, &id, &s, &values))
                    .await
                {
                    Ok(()) => return h,
                    Err(e) => format!("{e:#}"),
                }
            }
            Err(why) => why,
        };
        warn!(
            flow = def.name,
            firing,
            step,
            reason = why,
            "a step's $RELAY_OUT was not kept"
        );
        let data = json!({ "flow": def.name, "firing": firing, "step": step, "reason": why });
        let summary = format!("flow `{}` step `{step}`: {why}", def.name);
        self.say(&def, event_kind::FLOW_REFUSED, summary, data).await;
        Happened::Ended { step, ok: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_flow::state::OUT_FILE_MAX;

    #[test]
    fn a_step_is_handed_what_came_before_and_keeps_what_it_appended() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        let w = Work::of(&home, "prod-logs", "f1", "triage");
        assert_eq!(w.dir, home.firing_dir("prod-logs", "f1"));
        assert!(
            w.input.starts_with(w.dir.join(".relay")),
            "Relay's own corner, apart from the step's files"
        );
        w.prepare(&[("probe".into(), "3 new errors".into())]).unwrap();
        assert_eq!(
            std::fs::read_to_string(w.input.join("probe.out")).unwrap(),
            "3 new errors"
        );
        assert_eq!(w.appended().unwrap().as_deref(), Some(""), "there to append to");
        std::fs::write(&w.out, "found=3\n").unwrap();
        // Picked up again, as a lost run resumed is: what it appended stays.
        w.prepare(&[("probe".into(), "3 new errors".into())]).unwrap();
        assert_eq!(w.appended().unwrap().as_deref(), Some("found=3\n"));
        let names: Vec<_> = std::fs::read_dir(&w.input)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["probe.out"], "no temporary file is left behind");
    }

    #[test]
    fn what_was_appended_is_refused_when_too_big_or_not_text() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        let w = Work::of(&home, "f", "f1", "s");
        assert_eq!(w.appended().unwrap(), None, "a step never prepared appended nothing");
        w.prepare(&[]).unwrap();
        std::fs::write(&w.out, vec![b'a'; OUT_FILE_MAX + 1]).unwrap();
        assert!(w.appended().unwrap_err().contains("1 MB"));
        std::fs::write(&w.out, [0xff, 0xfe]).unwrap();
        assert!(w.appended().unwrap_err().contains("UTF-8"));
    }
}

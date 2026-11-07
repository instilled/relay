//! What `--detect <FILE>` holds: the patterns and timings a holder runs its
//! heuristics with, as JSON, so they can cross a process boundary.

use super::heuristics::{Patterns, Timing};
use anyhow::Context;
use regex::Regex;
use rl_core::state::RunKind;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectSpec {
    pub kind: RunKind,
    /// Regex sources.
    pub needs_input: Vec<String>,
    pub idle: Vec<String>,
    pub idle_after_ms: u64,
    pub stale_after_ms: u64,
}

impl DetectSpec {
    pub fn from_parts(kind: RunKind, patterns: &Patterns, timing: Timing) -> Self {
        let sources = |res: &[Regex]| res.iter().map(|r| r.as_str().to_string()).collect();
        Self {
            kind,
            needs_input: sources(&patterns.needs_input),
            idle: sources(&patterns.idle),
            idle_after_ms: timing.idle_after.as_millis() as u64,
            stale_after_ms: timing.stale_after.as_millis() as u64,
        }
    }

    /// Compiles the patterns; the error names the one that does not.
    pub fn compile(&self) -> anyhow::Result<(Patterns, Timing)> {
        let compile = |srcs: &[String]| -> anyhow::Result<Vec<Regex>> {
            srcs.iter()
                .map(|s| Regex::new(s).with_context(|| format!("bad pattern {s:?}")))
                .collect()
        };
        let patterns = Patterns {
            needs_input: compile(&self.needs_input)?,
            idle: compile(&self.idle)?,
        };
        let timing = Timing {
            idle_after: Duration::from_millis(self.idle_after_ms),
            stale_after: Duration::from_millis(self.stale_after_ms),
        };
        Ok((patterns, timing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_survives_json_and_compiles_back() {
        let spec = DetectSpec {
            kind: RunKind::Agent,
            needs_input: vec![r"\[y/N\]".into()],
            idle: vec![r"^❯\s*$".into()],
            idle_after_ms: 1500,
            stale_after_ms: 600_000,
        };
        let back: DetectSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        let (p, t) = back.compile().unwrap();
        assert_eq!(p.needs_input[0].as_str(), r"\[y/N\]");
        assert_eq!(t.idle_after, Duration::from_millis(1500));
        assert_eq!(DetectSpec::from_parts(RunKind::Agent, &p, t), spec);
    }

    #[test]
    fn a_bad_pattern_is_named() {
        let spec = DetectSpec {
            kind: RunKind::Agent,
            needs_input: vec!["(".into()],
            idle: vec![],
            idle_after_ms: 1,
            stale_after_ms: 1,
        };
        assert!(format!("{:#}", spec.compile().unwrap_err()).contains("("));
    }
}

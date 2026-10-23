//! What a run did, collected once for the report: its work, its cost, its words.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunFacts {
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub commits: u32,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub top_paths: Vec<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pushed: bool,
    #[serde(default)]
    pub active_secs: Option<u64>,
    #[serde(default)]
    pub tokens_in: Option<u64>,
    #[serde(default)]
    pub tokens_out: Option<u64>,
    #[serde(default)]
    pub errors: u32,
    #[serde(default)]
    pub recap: Option<String>,
    #[serde(default)]
    pub worked_on: Option<String>,
    #[serde(default)]
    pub questions: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub at: Timestamp,
}

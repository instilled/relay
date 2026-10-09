//! What a firing came to, and the method that asks for all of it.

use crate::methods::Method;
use serde::{Deserialize, Serialize};

/// What a firing came to: what its last step to run printed — the brief, the proposal, the
/// error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub firing: String,
    /// The last step, in the flow's order, that ran and printed something.
    pub step: String,
    /// ANSI stripped and trimmed; the last 64 KB.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiringRef {
    pub id: String,
}

/// What a firing came to, all of it; None while nothing has, or when there is no such firing.
pub struct FlowOutcome;
impl Method for FlowOutcome {
    const NAME: &'static str = "flow.outcome";
    type Params = FiringRef;
    type Result = Option<Outcome>;
}

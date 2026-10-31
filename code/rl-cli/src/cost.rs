//! What a summary may cost before it asks first.

/// More assistant calls than this and a summary over many runs asks first
/// (`/summary` in the panel, `rl summary` without `--yes`).
pub const ASK_OVER: u32 = 3;

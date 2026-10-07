//! Flows, decided: the flow file read, its steps planned, and what a firing
//! does next. No I/O — the daemon hands in what it read and carries out the
//! answer.

pub mod cond;
pub mod doc;
pub mod template;

//! The ledger: one SQLite file, one writer thread.
//!
//! Every access goes through [`Ledger::call`], which ships a closure to the
//! thread that owns the connection. That serialises writers (no `SQLITE_BUSY`
//! dance), keeps `rusqlite` types off async tasks, and makes ordering obvious:
//! a `call` that returns has been applied. Table families are plain functions
//! over `&Connection` in their own modules so they unit-test without the thread.

pub mod decisions;
pub mod events;
pub mod firings;
pub mod flows;
pub mod graph;
pub mod hidden;
pub mod kept;
pub mod meta;
pub mod migrate;
pub mod order;
pub mod presence;
pub mod runs;
pub mod session_io;
pub mod sessions;
pub mod summaries;
pub mod workspaces;

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use std::path::Path;
use std::sync::mpsc;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

#[derive(Clone)]
pub struct Ledger {
    tx: mpsc::Sender<Job>,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        migrate::run(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rl-ledger".into())
            .spawn(move || {
                for job in rx {
                    job(&mut conn);
                }
            })
            .context("spawning ledger thread")?;
        Ok(Self { tx })
    }

    /// Runs `f` on the ledger thread and awaits its result.
    pub async fn call<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("ledger thread is gone"))?;
        rx.await.context("ledger thread dropped the job")?
    }

    /// Same as [`call`](Self::call) for synchronous callers (tests, CLIs).
    pub fn call_blocking<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("ledger thread is gone"))?;
        rx.recv().context("ledger thread dropped the job")?
    }
}

/// Wraps a parse/convert failure as a rusqlite error at column `idx`, so row
/// mappers can stay inside `rusqlite::Result`.
pub(crate) fn conv<E>(idx: usize, e: E) -> rusqlite::Error
where
    E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, e.into())
}

pub(crate) fn conv_str(idx: usize, e: String) -> rusqlite::Error {
    conv(idx, std::io::Error::other(e))
}

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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

/// Tells apart the journal dirs of the many in-memory ledgers a test run opens.
static MEM_IO: AtomicU64 = AtomicU64::new(0);

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
        // Journal files sit beside `ledger.db`, in `io/` under the same home.
        let io_root = path.parent().map(|p| p.join("io")).unwrap_or_else(|| PathBuf::from("io"));
        Self::from_connection(conn, &io_root)
    }

    pub fn open_in_memory() -> Result<Self> {
        let io_root = std::env::temp_dir().join(format!(
            "rl-io-mem-{}-{}",
            std::process::id(),
            MEM_IO.fetch_add(1, Ordering::Relaxed)
        ));
        Self::from_connection(Connection::open_in_memory()?, &io_root)
    }

    fn from_connection(mut conn: Connection, io_root: &Path) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        migrate::run(&mut conn)?;
        // So any `&Connection` can find the journal files without a path
        // threaded through every caller.
        meta::set(&conn, session_io::IO_ROOT_KEY, &io_root.to_string_lossy())?;
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

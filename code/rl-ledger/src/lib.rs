//! The ledger: one SQLite file, one writer thread.
//!
//! Every access goes through [`Ledger::call`], which ships a closure to the
//! thread that owns the connection. That serialises writers (no `SQLITE_BUSY`
//! dance), keeps `rusqlite` types off async tasks, and makes ordering obvious:
//! a `call` that returns has been applied. Table families are plain functions
//! over `&Connection` in their own modules so they unit-test without the thread.

pub mod decisions;
pub mod dismissals;
pub mod events;
pub mod firings;
pub mod flows;
pub mod hidden;
pub mod kept;
pub mod leads;
pub mod meta;
pub mod migrate;
pub mod order;
pub mod presence;
pub mod runs;
pub mod session_io;
pub mod sessions;
pub mod summaries;
pub mod tags;
pub mod workspaces;

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// Tells apart the journal dirs of the many in-memory ledgers a test run opens.
static MEM_IO: AtomicU64 = AtomicU64::new(0);

/// Read-only connections beside the writer. WAL lets them read while it
/// writes, so a slow query (a digest, a summary) no longer stalls journaling.
const READERS: usize = 4;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;
type ReadJob = Box<dyn FnOnce(&Connection) + Send + 'static>;

#[derive(Clone)]
pub struct Ledger {
    tx: mpsc::Sender<Job>,
    /// Read jobs go here, to the read pool — `None` for an in-memory ledger,
    /// whose data is per-connection and so cannot be read from another handle;
    /// there, reads fall back to the writer thread.
    read_tx: Option<mpsc::Sender<ReadJob>>,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // Journal files sit beside `ledger.db`, in `io/` under the same home.
        let io_root = path
            .parent()
            .map(|p| p.join("io"))
            .unwrap_or_else(|| PathBuf::from("io"));
        Self::from_connection(conn, &io_root, Some(path))
    }

    pub fn open_in_memory() -> Result<Self> {
        let io_root = std::env::temp_dir().join(format!(
            "rl-io-mem-{}-{}",
            std::process::id(),
            MEM_IO.fetch_add(1, Ordering::Relaxed)
        ));
        Self::from_connection(Connection::open_in_memory()?, &io_root, None)
    }

    fn from_connection(mut conn: Connection, io_root: &Path, db_path: Option<&Path>) -> Result<Self> {
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
                // The writer connection stays open for the ledger's life, which
                // is what keeps the WAL live for the read-only connections.
                for job in rx {
                    job(&mut conn);
                }
            })
            .context("spawning ledger thread")?;
        // A file-backed ledger gets read-only connections beside the writer;
        // an in-memory one cannot (its data lives in the one connection).
        let read_tx = db_path.map(Self::spawn_readers).transpose()?;
        Ok(Self { tx, read_tx })
    }

    /// Opens `READERS` read-only connections to the same file, each on its own
    /// thread pulling from a shared queue. They exit when the last `Ledger`
    /// drops (every read sender gone), the way the writer thread does.
    fn spawn_readers(path: &Path) -> Result<mpsc::Sender<ReadJob>> {
        let (tx, rx) = mpsc::channel::<ReadJob>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..READERS {
            let conn =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                    .with_context(|| format!("opening {} read-only", path.display()))?;
            conn.pragma_update(None, "busy_timeout", 5000)?;
            conn.pragma_update(None, "query_only", "ON")?;
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("rl-ledger-read-{i}"))
                .spawn(move || {
                    loop {
                        // Hold the lock only to take the next job; run it
                        // unlocked so the readers work in parallel.
                        let job = {
                            let guard = rx.lock().unwrap();
                            guard.recv()
                        };
                        match job {
                            Ok(job) => job(&conn),
                            Err(_) => break,
                        }
                    }
                })
                .context("spawning ledger read thread")?;
        }
        Ok(tx)
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

    /// Runs a read `f` on a read-only connection from the pool, off the writer
    /// thread, and awaits its result. Use it for queries that only read; a
    /// write would be refused by the read-only connection. Falls back to the
    /// writer thread for an in-memory ledger, which has no pool.
    ///
    /// It sees the latest *committed* state, so it suits the display and query
    /// surface (the digest, events, summaries, scrollback) where a moment's lag
    /// is fine. It is NOT for a reader that must observe a write another task
    /// just issued but has not awaited: the pool breaks the writer queue's
    /// total ordering, so such a read can land before that write commits. Those
    /// reads — the flow engine's own, mid-reconcile — stay on [`call`].
    pub async fn call_read<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R> + Send + 'static,
    {
        let Some(read_tx) = &self.read_tx else {
            return self.call(move |c| f(c)).await;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        read_tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("ledger read pool is gone"))?;
        rx.await.context("ledger read pool dropped the job")?
    }

    /// [`call_read`](Self::call_read) for synchronous callers (tests, CLIs).
    pub fn call_read_blocking<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R> + Send + 'static,
    {
        let Some(read_tx) = &self.read_tx else {
            return self.call_blocking(move |c| f(c));
        };
        let (tx, rx) = mpsc::channel();
        read_tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("ledger read pool is gone"))?;
        rx.recv().context("ledger read pool dropped the job")?
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

//! What keeps the list live while it is open, and nothing once it is closed:
//! a watch on the session and hook folders, and a background task asking the
//! harnesses for their own sessions. Both stop when they are dropped.

use crate::attach::AbortOnDrop;
use crate::sources::{NativeLister, NativeRow};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rl_core::Home;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// A change under `sessions/` or `state/`, said once per change the system
/// reports; the list gathers them (`DEBOUNCE`) before it reads again.
pub struct Watch {
    _watcher: RecommendedWatcher,
    rx: mpsc::UnboundedReceiver<()>,
}

/// How long the list gathers changes before it reads the files again: one
/// read for a burst of writes, and never more than this late.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

impl Watch {
    /// Watches `sessions/` and `state/`, and everything under them. They are
    /// made if they are not there.
    pub fn start(home: &Home) -> notify::Result<Watch> {
        home.ensure_store()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if ev.is_ok() {
                let _ = tx.send(());
            }
        })?;
        for dir in [home.sessions_dir(), home.state_dir()] {
            // The system reports changes by the real path.
            let dir = dir.canonicalize().unwrap_or(dir);
            watcher.watch(&dir, RecursiveMode::Recursive)?;
        }
        Ok(Watch { _watcher: watcher, rx })
    }

    /// The next change; the ones already waiting with it are taken too.
    /// Never, once the watch has stopped.
    pub async fn changed(&mut self) {
        if self.rx.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
        while self.rx.try_recv().is_ok() {}
    }
}

/// The harnesses' own sessions, asked for every `every` in the background,
/// for as long as this is kept.
pub struct Native {
    rx: watch::Receiver<Vec<NativeRow>>,
    _task: Option<AbortOnDrop<()>>,
}

impl Native {
    /// Asks each lister now and then every `every`, one after the other, off
    /// the async threads. A lister that fails keeps what it said last. With
    /// none, nothing runs.
    pub fn start(listers: Vec<Box<dyn NativeLister>>, every: Duration) -> Native {
        let (tx, rx) = watch::channel(Vec::new());
        if listers.is_empty() {
            return Native { rx, _task: None };
        }
        let listers: Arc<Vec<Box<dyn NativeLister>>> = Arc::new(listers);
        let task = tokio::spawn(async move {
            let mut kept: Vec<Vec<NativeRow>> = vec![Vec::new(); listers.len()];
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let asked = listers.clone();
                let Ok(said) =
                    tokio::task::spawn_blocking(move || asked.iter().map(|l| l.list().ok()).collect::<Vec<_>>()).await
                else {
                    continue;
                };
                for (k, s) in kept.iter_mut().zip(said) {
                    if let Some(rows) = s {
                        *k = rows;
                    }
                }
                let all = kept.concat();
                tx.send_if_modified(|now| {
                    let changed = *now != all;
                    if changed {
                        *now = all;
                    }
                    changed
                });
            }
        });
        Native {
            rx,
            _task: Some(AbortOnDrop(task)),
        }
    }

    /// Once the rows have changed since they were last taken. Never, with
    /// nothing to ask.
    pub async fn changed(&mut self) {
        if self.rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// The rows, as last said.
    pub fn rows(&mut self) -> Vec<NativeRow> {
        self.rx.borrow_and_update().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::NativeState;
    use rl_core::state::RunState;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn row(id: &str) -> NativeRow {
        NativeRow {
            harness: "claude".into(),
            id: id.into(),
            session_id: format!("s-{id}"),
            name: id.into(),
            cwd: "/w".into(),
            state: NativeState {
                words: "working".into(),
                state: RunState::Working,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
        }
    }

    /// Says what it is told to, or fails when told nothing; counts its asks.
    struct Told(Arc<Mutex<Option<Vec<NativeRow>>>>, Arc<AtomicUsize>);

    impl NativeLister for Told {
        fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no answer"))
        }
    }

    #[tokio::test]
    async fn the_listers_are_asked_in_the_background_and_a_failure_keeps_the_last_word() {
        let said = Arc::new(Mutex::new(Some(vec![row("a")])));
        let asked = Arc::new(AtomicUsize::new(0));
        let mut native = Native::start(
            vec![Box::new(Told(said.clone(), asked.clone()))],
            Duration::from_millis(20),
        );
        tokio::time::timeout(Duration::from_secs(5), native.changed())
            .await
            .unwrap();
        assert_eq!(native.rows(), vec![row("a")]);
        // It fails: what it said last stands.
        *said.lock().unwrap() = None;
        let before = asked.load(Ordering::SeqCst);
        while asked.load(Ordering::SeqCst) < before + 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(native.rows(), vec![row("a")]);
        // It says something new.
        *said.lock().unwrap() = Some(vec![row("b")]);
        tokio::time::timeout(Duration::from_secs(5), native.changed())
            .await
            .unwrap();
        assert_eq!(native.rows(), vec![row("b")]);
        // Dropped, it asks no more.
        drop(native);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let then = asked.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            asked.load(Ordering::SeqCst),
            then,
            "nothing runs once the list is closed"
        );
    }

    #[tokio::test]
    async fn with_no_listers_nothing_is_asked_and_nothing_changes() {
        let mut native = Native::start(Vec::new(), Duration::from_millis(10));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), native.changed())
                .await
                .is_err()
        );
        assert!(native.rows().is_empty());
    }

    #[tokio::test]
    async fn a_write_under_sessions_or_state_is_seen() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        let mut w = Watch::start(&home).unwrap();
        for (i, dir) in [home.session_dir("api-sh"), home.state_dir().join("claude")]
            .into_iter()
            .enumerate()
        {
            // What the last write still has on its way is not this one's.
            tokio::time::sleep(Duration::from_millis(300)).await;
            while w.rx.try_recv().is_ok() {}
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{i}.json")), "{}").unwrap();
            tokio::time::timeout(Duration::from_secs(10), w.changed())
                .await
                .unwrap_or_else(|_| panic!("no change seen under {}", dir.display()));
        }
    }
}

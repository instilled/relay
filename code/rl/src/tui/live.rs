//! What keeps the list live while it is open, and nothing once it is closed:
//! a watch on the session and hook folders, and a background task asking the
//! harnesses for their own sessions. Both stop when they are dropped.

use crate::attach::AbortOnDrop;
use crate::sources::{NativeLister, NativeRow};
use jiff::Timestamp;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rl_core::Home;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, watch};

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

/// What the harnesses' listers last said, all told.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Said {
    /// Their sessions, as each last listed them.
    pub rows: Vec<NativeRow>,
    /// The harnesses whose lister has answered at least once.
    pub answered: BTreeSet<String>,
    /// The listers whose last ask failed, while it is their last word.
    pub failing: Vec<Failing>,
}

/// A lister whose last ask failed: what its rows still show is from `since`.
#[derive(Debug, Clone, PartialEq)]
pub struct Failing {
    pub harness: String,
    pub error: String,
    /// When it last answered; never, if it has not.
    pub since: Option<Timestamp>,
}

/// One lister's last word.
#[derive(Clone, Default)]
struct Kept {
    rows: Vec<NativeRow>,
    answered: Option<Timestamp>,
    error: Option<String>,
}

/// The harnesses' own sessions, asked for every `every` in the background,
/// for as long as this is kept — and at once when `ask_now` says; not while
/// it is paused.
pub struct Native {
    rx: watch::Receiver<Said>,
    ask: Arc<Notify>,
    paused: Arc<AtomicBool>,
    _task: Option<AbortOnDrop<()>>,
}

impl Native {
    /// Asks each lister now and then every `every`, one after the other, off
    /// the async threads. A lister that fails keeps what it said last, and
    /// says why until it answers again. With none, nothing runs.
    pub fn start(listers: Vec<Box<dyn NativeLister>>, every: Duration) -> Native {
        let (tx, rx) = watch::channel(Said::default());
        let ask = Arc::new(Notify::new());
        let paused = Arc::new(AtomicBool::new(false));
        if listers.is_empty() {
            return Native {
                rx,
                ask,
                paused,
                _task: None,
            };
        }
        let listers: Arc<Vec<Box<dyn NativeLister>>> = Arc::new(listers);
        let asked_now = ask.clone();
        let held = paused.clone();
        let task = tokio::spawn(async move {
            let mut kept: Vec<Kept> = vec![Kept::default(); listers.len()];
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    () = asked_now.notified() => {}
                }
                if held.load(Ordering::SeqCst) {
                    continue;
                }
                let asked = listers.clone();
                let Ok(said) = tokio::task::spawn_blocking(move || {
                    asked
                        .iter()
                        .map(|l| l.list().map_err(|e| format!("{e:#}")))
                        .collect::<Vec<_>>()
                })
                .await
                else {
                    continue;
                };
                let now = Timestamp::now();
                for (k, s) in kept.iter_mut().zip(said) {
                    match s {
                        Ok(rows) => {
                            k.rows = rows;
                            k.answered = Some(now);
                            k.error = None;
                        }
                        Err(e) => k.error = Some(e),
                    }
                }
                let all = Said {
                    rows: kept.iter().flat_map(|k| k.rows.clone()).collect(),
                    answered: listers
                        .iter()
                        .zip(&kept)
                        .filter(|(_, k)| k.answered.is_some())
                        .map(|(l, _)| l.harness().to_string())
                        .collect(),
                    failing: listers
                        .iter()
                        .zip(&kept)
                        .filter_map(|(l, k)| {
                            Some(Failing {
                                harness: l.harness().to_string(),
                                error: k.error.clone()?,
                                since: k.answered,
                            })
                        })
                        .collect(),
                };
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
            ask,
            paused,
            _task: Some(AbortOnDrop(task)),
        }
    }

    /// Asks the listers now, not at the next tick: a session was just
    /// started in a harness's background.
    pub fn ask_now(&self) {
        self.ask.notify_one();
    }

    /// No asking while the terminal is a session's: what the harness's CLI
    /// does then is not to run beside it.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Asking again, at once.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.ask_now();
    }

    /// Once the rows have changed since they were last taken. Never, with
    /// nothing to ask.
    pub async fn changed(&mut self) {
        if self.rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// What they last said.
    pub fn said(&mut self) -> Said {
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
        fn harness(&self) -> &str {
            "claude"
        }

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
        let first = native.said();
        assert_eq!(first.rows, vec![row("a")]);
        assert!(first.answered.contains("claude") && first.failing.is_empty());
        // It fails: what it said last stands, and why it is not newer.
        *said.lock().unwrap() = None;
        tokio::time::timeout(Duration::from_secs(5), native.changed())
            .await
            .unwrap();
        let failed = native.said();
        assert_eq!(failed.rows, vec![row("a")]);
        assert!(failed.answered.contains("claude"), "it has answered before");
        assert_eq!(failed.failing.len(), 1);
        assert_eq!(
            (failed.failing[0].harness.as_str(), failed.failing[0].error.as_str()),
            ("claude", "no answer")
        );
        assert!(failed.failing[0].since.is_some(), "since its last answer");
        // It says something new: the failure is over.
        *said.lock().unwrap() = Some(vec![row("b")]);
        tokio::time::timeout(Duration::from_secs(5), native.changed())
            .await
            .unwrap();
        let again = native.said();
        assert_eq!(again.rows, vec![row("b")]);
        assert!(again.failing.is_empty());
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
    async fn asked_now_it_does_not_wait_for_the_tick() {
        let said = Arc::new(Mutex::new(Some(vec![row("a")])));
        let asked = Arc::new(AtomicUsize::new(0));
        let native = Native::start(
            vec![Box::new(Told(said.clone(), asked.clone()))],
            Duration::from_secs(3600),
        );
        let count = |n: usize| {
            let asked = asked.clone();
            async move {
                while asked.load(Ordering::SeqCst) < n {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        };
        // The first tick is at once; the next an hour on.
        tokio::time::timeout(Duration::from_secs(5), count(1)).await.unwrap();
        native.ask_now();
        tokio::time::timeout(Duration::from_secs(5), count(2))
            .await
            .expect("asked at once");
    }

    #[tokio::test]
    async fn a_lister_that_never_answered_is_not_answered() {
        let said = Arc::new(Mutex::new(None));
        let asked = Arc::new(AtomicUsize::new(0));
        let mut native = Native::start(
            vec![Box::new(Told(said.clone(), asked.clone()))],
            Duration::from_millis(20),
        );
        tokio::time::timeout(Duration::from_secs(5), native.changed())
            .await
            .unwrap();
        let s = native.said();
        assert!(s.answered.is_empty() && s.rows.is_empty());
        assert_eq!(s.failing[0].since, None, "nothing shown is from it");
    }

    #[tokio::test]
    async fn paused_it_asks_nothing_and_resumed_it_asks_at_once() {
        let said = Arc::new(Mutex::new(Some(vec![row("a")])));
        let asked = Arc::new(AtomicUsize::new(0));
        let native = Native::start(
            vec![Box::new(Told(said.clone(), asked.clone()))],
            Duration::from_millis(20),
        );
        while asked.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        native.pause();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let then = asked.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(asked.load(Ordering::SeqCst), then, "nothing asked while attached");
        native.resume();
        tokio::time::timeout(Duration::from_secs(5), async {
            while asked.load(Ordering::SeqCst) <= then {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("asked again");
    }

    #[tokio::test]
    async fn with_no_listers_nothing_is_asked_and_nothing_changes() {
        let mut native = Native::start(Vec::new(), Duration::from_millis(10));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), native.changed())
                .await
                .is_err()
        );
        assert_eq!(native.said(), Said::default());
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

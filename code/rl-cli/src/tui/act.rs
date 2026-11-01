//! `impl App` — the run & project actions cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// `/rename`, `^r`: whatever the cursor is on, by the name it is known by —
    /// a folder through the registry, a run (a shell is one) through the
    /// ledger, both typed over where they stand in the list. A flow is its
    /// folder's name in `~/.relay/flows`.
    pub(super) fn rename_here(&mut self) {
        // A name is changed where it stands, and it stands on the list.
        if self.view == View::CatchUp {
            return self.say("^r renames on the list · esc goes there");
        }
        let what = if let Some(folder) = self.selected_project() {
            Some((self.names.workspace(&folder), Renamed::Project(folder)))
        } else {
            self.selected_run()
                .map(|i| (i.run.title.clone(), Renamed::Run(i.run.id.clone())))
        };
        if self.on_everywhere() {
            self.say(format!("{} · nothing to rename", Self::NOT_A_PROJECT));
        } else if let (Some((was, what)), Some(key)) = (what, self.sel.clone()) {
            // The keys are the name's now: a search gives the line back as
            // `enter` would, and what the line offered is put away.
            self.search = None;
            self.completing = None;
            self.pick = None;
            self.mode = Mode::Rename(Box::new(Renaming {
                key,
                what,
                field: line::Field::selected(was.clone()),
                was,
                from: Default::default(),
            }));
        } else if let Some(name) = self.flow_here() {
            self.say(format!("{name} is its folder's name in ~/.relay/flows"));
        } else {
            self.say("^r renames what the cursor is on");
        }
    }

    /// "Not this". Whatever is typed on the line goes with it, as the reason.
    pub(super) async fn dismiss(&mut self, id: String) {
        let note = Some(self.line.text().trim().to_string()).filter(|n| !n.is_empty());
        match self
            .rpc
            .call::<DecisionDismiss>(DecisionNote { id, note: note.clone() })
            .await
        {
            Ok(_) => {
                if note.is_some() {
                    self.line.clear();
                }
                self.say("dismissed");
                self.refresh().await;
            }
            Err(e) => self.say(format!("dismiss: {e:#}")),
        }
    }

    /// Asks a live run to stop; with `archive`, the daemon also takes it off
    /// the list once it has ended (`^x`), so one ask does both.
    pub(super) async fn stop(&mut self, id: String, archive: bool) {
        match self
            .rpc
            .call::<RunStop>(RunStopParams {
                id: id.clone(),
                force: false,
                archive,
            })
            .await
        {
            Ok(_) if archive => self.say("stopping · off the list once it has ended"),
            Ok(_) => self.say("stop requested"),
            Err(e) => self.say(format!("stop: {e:#}")),
        }
    }

    pub(super) async fn mark(&mut self, id: String, mark: Mark) {
        match self.rpc.call::<RunMark>(RunMarkParams { id, mark, note: None }).await {
            Ok(r) => {
                self.say(format!("marked {}", rows::state_words(r.state, r.confidence)));
                self.refresh().await;
            }
            Err(e) => self.say(format!("mark: {e:#}")),
        }
    }

    /// Done with these ended runs: out of the list, kept in the ledger.
    pub(super) async fn archive(&mut self, ids: Vec<String>, what: &str) {
        match self
            .rpc
            .call::<RunArchive>(RunArchiveParams { ids, all_seen: false })
            .await
        {
            Ok(0) => self.say("nothing to archive: still going"),
            Ok(_) => {
                self.say(format!("archived {what}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("archive: {e:#}")),
        }
    }

    /// Whether the cursor is on a flow in the catch-up, which holds only the
    /// stretch's runs: archiving it is its project's overview's, where it
    /// holds them all (`FLOW_ON_THE_LIST`).
    pub(super) fn flow_in_catch_up(&self) -> bool {
        self.view == View::CatchUp && self.selected_flow().is_some()
    }

    /// Archives a flow's runs, however they ended: out of the list, kept in
    /// the ledger. Shared by `^x` (`remove_selected`) and `/archive`. Takes the
    /// name and run ids rather than `&rows::FlowRow` itself: it borrows from
    /// `self`, and this needs `&mut self` to archive.
    pub(super) async fn archive_flow(&mut self, name: &str, ids: Vec<String>) {
        if ids.is_empty() {
            return self.say(format!("nothing to archive: {name} has not run"));
        }
        let what = format!("{name} · {} runs", ids.len());
        self.archive(ids, &what).await;
    }

    /// The second `ctrl-x` (the first only armed it): get whatever the cursor
    /// is on out of the list. An ended run, or a flow's runs, are archived
    /// (reversible: the ledger keeps them); a run still going is stopped and
    /// archived as soon as it has ended, killed if it lingers; a question is
    /// dismissed; a project is removed, its runs stopped and archived. Nothing
    /// here is destroyed — that is retention's alone.
    pub(super) async fn remove_selected(&mut self) {
        if let Some(f) = self.selected_flow() {
            let ids: Vec<String> = f.runs.iter().map(|i| i.run.id.clone()).collect();
            self.archive_flow(&f.name, ids).await;
        } else if let Some(i) = self.selected_run() {
            let (id, title) = (i.run.id.clone(), i.run.title.clone());
            if i.run.state.is_terminal() {
                self.archive(vec![id], &title).await;
            } else {
                self.stop(id, true).await;
            }
        } else if let Some(q) = self.selected_question() {
            self.dismiss(q.id.clone()).await;
        } else if self.on_everywhere() {
            self.say(format!("{} · nothing to remove", Self::NOT_A_PROJECT));
        } else if let Some(Row::Group(h)) = self.selected() {
            let (folder, name) = (h.workspace.clone(), h.name.clone());
            self.remove_project(folder, name);
        } else {
            self.say("nothing here to remove");
        }
    }

    /// The second `^x` on a project's header: the project goes at once.
    pub(super) fn remove_project(&mut self, folder: PathBuf, name: String) {
        if self.removing.is_some() {
            self.say("still removing a project; a moment");
            return;
        }
        self.start_removal(folder, name);
    }

    /// `^x` on a project's header: open the typed confirm. Taking a whole
    /// project down archives all its runs, so it is not armed and fired by a
    /// second keypress — a stray or laggy `^x` cannot do it; you type `DELETE`.
    pub(super) fn confirm_remove_project(&mut self) {
        if self.removing.is_some() {
            return self.say("still removing a project; a moment");
        }
        let Some(Row::Group(h)) = self.selected() else {
            return self.say("nothing here to remove");
        };
        let (workspace, name) = (h.workspace.clone(), h.name.clone());
        let plan = remove::arm(&remove::Plan::of(&name, &workspace, &self.digest.items));
        self.mode = Mode::Confirm(Box::new(Confirming {
            workspace,
            name,
            plan,
            field: line::Field::selected(""),
        }));
    }

    /// Hands the removal to the daemon on a connection of its own, so the
    /// panel keeps drawing while the project's runs are stopped.
    pub(super) fn start_removal(&mut self, folder: PathBuf, name: String) {
        let home = self.home.clone();
        let params = WorkspaceRemoveParams {
            path: folder,
            stop_runs: true,
            archive_runs: true,
        };
        let task = tokio::spawn(async move {
            let mut rpc = Client::connect(&home).await?;
            rpc.call::<WorkspaceRemove>(params).await
        });
        self.removing = Some(Removing { name, task });
    }

    pub(super) async fn removed(
        &mut self,
        done: std::result::Result<Result<WorkspaceRemoved>, tokio::task::JoinError>,
    ) {
        let Some(r) = self.removing.take() else { return };
        match done {
            Ok(Ok(out)) => self.say(remove::done(&r.name, &out)),
            Ok(Err(e)) => self.say(format!("remove {}: {e:#}", r.name)),
            Err(e) => self.say(format!("remove {}: {e}", r.name)),
        }
        self.refresh().await;
    }

    /// What a second `ctrl-x` would do to the row under the cursor, for the
    /// armed hint — `None` when there is nothing to remove there.
    pub(super) fn remove_hint(&self) -> Option<String> {
        if let Some(f) = self.selected_flow() {
            // One that has not run has nothing to archive.
            (!f.runs.is_empty()).then(|| format!("remove {} · {} runs", f.name, f.runs.len()))
        } else if let Some(i) = self.selected_run() {
            if i.run.state.is_terminal() {
                Some(format!("remove {}", i.run.title))
            } else {
                Some(format!("stop and remove {}", i.run.title))
            }
        } else if let Some(q) = self.selected_question() {
            Some(format!("dismiss {}", q.title))
        } else if self.on_everywhere() {
            None
        } else if let Some(Row::Group(h)) = self.selected() {
            Some(remove::arm(&remove::Plan::of(
                &h.name,
                &h.workspace,
                &self.digest.items,
            )))
        } else {
            None
        }
    }

    /// Done with everything ended that has been looked at.
    pub(super) async fn archive_seen(&mut self) {
        match self
            .rpc
            .call::<RunArchive>(RunArchiveParams {
                ids: vec![],
                all_seen: true,
            })
            .await
        {
            Ok(n) => {
                self.say(format!("archived {n}"));
                self.refresh().await;
            }
            Err(e) => self.say(format!("archive: {e:#}")),
        }
    }

    /// Register a folder as a project from the line — the panel's `rl up`. The
    /// cursor lands on the new row (empty until something runs there, thanks
    /// to project rows).
    pub(super) async fn add_dir(&mut self, arg: &str) -> Result<()> {
        let arg = arg.trim();
        if arg.is_empty() {
            self.say("a folder: /up <path>");
            return Ok(());
        }
        let expanded = match arg.strip_prefix('~') {
            Some(rest) => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
                None => PathBuf::from(arg),
            },
            None => PathBuf::from(arg),
        };
        let path = match std::fs::canonicalize(&expanded) {
            Ok(p) => p,
            Err(_) => {
                self.say(format!("no such folder: {}", expanded.display()));
                return Ok(());
            }
        };
        self.rpc
            .call::<WorkspaceUp>(WorkspaceUpParams { path: path.clone() })
            .await?;
        self.refresh().await;
        self.say(format!("added {}", self.names.workspace(&path)));
        self.select(Some(format!("g:{}", path.display())));
        Ok(())
    }

    /// Give a project a new name — or, given none, its folder's own back, the
    /// one `rl up` registered it under. The cursor stays on it.
    pub(super) async fn rename_project(&mut self, folder: PathBuf, name: String) -> Result<()> {
        let back = name.is_empty();
        let name = if back {
            let own = folder.file_name().unwrap_or(folder.as_os_str());
            own.to_string_lossy().into_owned()
        } else {
            name
        };
        self.rpc
            .call::<WorkspaceRename>(WorkspaceRenameParams {
                path: folder.clone(),
                name: name.clone(),
            })
            .await?;
        self.say(format!("{} {name}", if back { "back to" } else { "renamed to" }));
        self.refresh().await;
        self.select(Some(format!("g:{}", folder.display())));
        Ok(())
    }

    /// `^p`: the run under the cursor to the top of the list, or back among
    /// the rest. The cursor goes with it.
    pub(super) async fn toggle_pin(&mut self) {
        let Some((id, pinned)) = self
            .selected_run()
            .map(|i| (i.run.id.clone(), i.run.pinned_at.is_some()))
        else {
            self.say("^t pins a run");
            return;
        };
        match self
            .rpc
            .call::<RunPin>(RunPinParams {
                id: id.clone(),
                pinned: !pinned,
            })
            .await
        {
            Ok(r) => self.say(if pinned {
                format!("unpinned {}", r.title)
            } else {
                format!("pinned {}", r.title)
            }),
            Err(e) => self.say(format!("pin: {e}")),
        }
        self.refresh().await;
        self.select(Some(format!("r:{id}")));
    }

    /// `/hide`: the row under the cursor off the list — a run, going or
    /// ended; a flow, its firings to come too; a project and everything in it.
    /// Nothing is stopped or removed. With hidden on it is the way back: the
    /// row comes back, and a header that is not hidden itself brings back what
    /// was hidden under it. A question is never hidden: it needs you.
    pub(super) async fn hide(&mut self) {
        // What it hides, and where the cursor goes after, is the list's.
        if self.view == View::CatchUp {
            return self.say("/hide hides on the list · esc goes there");
        }
        let back = self.show_hidden;
        let rows = self.rows();
        let Some(at) = self
            .sel
            .as_ref()
            .and_then(|k| rows.iter().position(|r| r.key().as_ref() == Some(k)))
        else {
            self.say("nothing here to hide");
            return;
        };
        // What to hide or bring back: runs by id, projects and flows by key.
        let mut ids: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        let hidden = |k: &str| self.digest.hidden.iter().any(|h| h.key == k);
        let own_hidden = |i: &DigestItem| i.run.hidden_at.is_some();
        let mut still: Option<&str> = None;
        let what = match &rows[at] {
            Row::Question(_) => {
                self.say("a question is never hidden: it needs you · answer it, or ^x lets it go");
                return;
            }
            Row::Run(i) => {
                if back {
                    still = rows::hidden_with(&i.run, &self.names, &self.flows, &self.digest.hidden);
                    if own_hidden(i) {
                        ids.push(i.run.id.clone());
                    }
                } else {
                    ids.push(i.run.id.clone());
                }
                i.run.title.clone()
            }
            Row::Flow(f) => {
                if !back || hidden(&f.key) {
                    keys.push(f.key.clone());
                }
                if back {
                    ids.extend(f.runs.iter().filter(|i| own_hidden(i)).map(|i| i.run.id.clone()));
                }
                f.name.clone()
            }
            Row::Outside(c) => {
                let key = c.key();
                if !back || hidden(&key) {
                    keys.push(key);
                } else {
                    still = Some("its project");
                }
                c.title()
            }
            Row::Group(h) => {
                let key = rows[at].key().unwrap_or_default();
                if !back || hidden(&key) {
                    keys.push(key);
                } else {
                    // Not hidden itself: what was hidden under it comes back.
                    for r in rows[at + 1..].iter().take_while(|r| !matches!(r, Row::Group(_))) {
                        match r {
                            Row::Run(i) if own_hidden(i) => ids.push(i.run.id.clone()),
                            Row::Flow(f) => {
                                if hidden(&f.key) {
                                    keys.push(f.key.clone());
                                }
                                ids.extend(f.runs.iter().filter(|i| own_hidden(i)).map(|i| i.run.id.clone()));
                            }
                            Row::Outside(c) if hidden(&c.key()) => keys.push(c.key()),
                            _ => {}
                        }
                    }
                }
                h.name.clone()
            }
        };
        if ids.is_empty() && keys.is_empty() {
            let why = still.map_or_else(String::new, |w| format!(": it is hidden with {w} · /hide on that"));
            self.say(format!("nothing to bring back in {what}{why}"));
            return;
        }
        // Where the cursor goes once the row has left: the next one, else the
        // one before.
        let next = rows[at + 1..]
            .iter()
            .chain(rows[..at].iter().rev())
            .find_map(|r| r.key());
        let here = rows[at].key();
        drop(rows);
        let mut failed = None;
        for id in ids {
            if let Err(e) = self.rpc.call::<RunHide>(RunHideParams { id, hidden: !back }).await {
                failed = Some(e);
            }
        }
        for key in keys {
            if let Err(e) = self
                .rpc
                .call::<DigestHide>(DigestHideParams { key, hidden: !back })
                .await
            {
                failed = Some(e);
            }
        }
        match failed {
            Some(e) => self.say(format!("hide: {e:#}")),
            None if back => self.say(match still {
                Some(w) => format!("brought back {what} · still hidden with {w}"),
                None => format!("brought back {what}"),
            }),
            None => self.say(format!("hid {what} · h on the list shows it")),
        }
        self.refresh().await;
        if self.keys().contains(here.as_ref().unwrap_or(&String::new())) {
            self.select(here);
        } else if next.as_ref().is_some_and(|k| self.keys().contains(k)) {
            self.select(next);
        }
    }

    /// Shift-↑/↓: the row under the cursor one place up or down inside its
    /// project, which keeps that order from then on (`digest.order`) — or,
    /// on a header, the project one place up or down among the projects. The
    /// cursor stays on the row, and holding the key goes on moving it. The
    /// order kept comes back at once, so the next press builds on it without
    /// waiting for the list to be fetched again.
    pub(super) async fn reorder(&mut self, up: bool) {
        match self.view {
            View::Work => {}
            View::Events => return self.say("⇧↑↓ moves rows on the list · events stay in the order they came"),
            View::CatchUp | View::Report | View::Overview => {
                return self.say("⇧↑↓ moves rows on the list · esc goes there");
            }
        }
        if self.show_hidden {
            self.say("with hidden on the list keeps its order · move it with hidden off");
            return;
        }
        let Some(key) = self.sel.clone() else {
            self.say("nothing here to move");
            return;
        };
        let order = {
            let shown = self.rows();
            let full = rows::build(
                &self.digest,
                &rows::Context {
                    names: &self.names,
                    flows: &self.flows,
                    filter: "",
                    show_flows: false,
                    show_agents: true,
                    show_shells: true,
                    show_hidden: false,
                    now: Timestamp::now(),
                    relay: Some(&self.relay),
                },
            );
            rows::reorder(&shown, &full, &self.digest.order, &key, up)
        };
        match order {
            Ok(order) => match self.rpc.call::<DigestOrder>(order).await {
                Ok(all) => self.digest.order = all,
                Err(e) => self.say(format!("reorder: {e:#}")),
            },
            Err(why) => self.say(why),
        }
    }

    /// `/sort`: the project under the cursor — its header, or any row in it
    /// — lets go of the order you put its rows in, and they go back to the
    /// order they came in. Where the project stands among the others stays.
    pub(super) async fn sort_project(&mut self) {
        if self.view == View::CatchUp {
            return self.say("/sort sorts on the list · esc goes there");
        }
        let found = {
            let rows = self.rows();
            let at = self
                .sel
                .as_ref()
                .and_then(|k| rows.iter().position(|r| r.key().as_ref() == Some(k)));
            at.and_then(|at| rows[..=at].iter().rposition(|r| matches!(r, Row::Group(_))))
                .and_then(|h| match &rows[h] {
                    Row::Group(head) => Some((rows[h].key().unwrap_or_default(), head.name.clone())),
                    _ => None,
                })
        };
        let Some((group, name)) = found else {
            self.say("/sort is for a project: on its header, it puts its rows back in the order they came");
            return;
        };
        if !self.digest.order.iter().any(|o| o.group == group) {
            self.say(format!(
                "{name} already stands in the order its rows came · ⇧↑↓ puts them in yours"
            ));
            return;
        }
        match self
            .rpc
            .call::<DigestOrder>(rl_proto::methods::Order {
                group,
                rows: Vec::new(),
            })
            .await
        {
            Ok(all) => {
                self.digest.order = all;
                self.say(format!(
                    "{name} goes back to the order its rows came in · the newest at the bottom"
                ));
            }
            Err(e) => self.say(format!("sort: {e:#}")),
        }
    }

    /// A run's own name, which is its title. The ledger is the only place it
    /// lives, so the rename is one call and the next refresh shows it. Given
    /// none, the run goes back to the name Relay gave it.
    pub(super) async fn rename_run(&mut self, id: String, title: String) -> Result<()> {
        let back = title.is_empty();
        let r = self
            .rpc
            .call::<RunRename>(RunRenameParams { id: id.clone(), title })
            .await?;
        self.say(format!("{} {}", if back { "back to" } else { "renamed to" }, r.title));
        self.refresh().await;
        self.select(Some(format!("r:{id}")));
        Ok(())
    }
}

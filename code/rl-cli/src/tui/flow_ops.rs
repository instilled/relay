//! `impl App` — the flow commands & reload cluster, lifted out of `mod.rs` verbatim.
//! See `tui/mod.rs` for the `App` struct these methods belong to.

use super::*;

impl App {
    /// The flow, found from elsewhere (the events, the catch-up): its
    /// project's overview — `everywhere`'s, with no `in` — with the cursor on
    /// it, opened from the project's line on the list, where `esc` goes back
    /// to. A search that hides it there lets go, as `show_run`'s does.
    pub(super) async fn show_flow(&mut self, name: &str) {
        let Some(flow) = self.flows.iter().find(|f| f.name == name && f.removed_at.is_none()) else {
            return self.say(format!("{name} is not a flow any more"));
        };
        let scope = overview::Scope::of(flow);
        let key = rows::flow_key(name);
        self.back_to_list().await;
        self.select(Some(scope.line_key()));
        self.open_overview(scope, Some(key.clone()));
        self.fetch_overview_top().await;
        if self.sel.as_ref() != Some(&key) && !self.filter.is_empty() {
            self.filter.clear();
            self.select(Some(key.clone()));
            self.say(format!("the search lets go, to show {name}"));
        }
        self.moved().await;
    }

    /// `/flow run|pause|resume|approve <name>`: the daemon's, then one line
    /// saying it.
    pub(super) async fn flow_act(&mut self, verb: &str, name: String) {
        let named = FlowName { name: name.clone() };
        let done = match verb {
            "run" => self
                .rpc
                .call::<FlowRun>(named)
                .await
                .map(|f| format!("fired {name} · firing {}", f.id)),
            "pause" => self
                .rpc
                .call::<FlowPause>(named)
                .await
                .map(|_| format!("{name} paused")),
            "resume" => self
                .rpc
                .call::<FlowResume>(named)
                .await
                .map(|_| format!("{name} resumed")),
            // The yes is to the version shown here: one loaded since is refused.
            "approve" => {
                let shown = self.flows.iter().find(|f| f.name == name);
                let hash = shown.map(|f| f.hash.clone()).unwrap_or_default();
                self.rpc
                    .call::<FlowApprove>(FlowApproval {
                        name: name.clone(),
                        hash,
                    })
                    .await
                    .map(|_| format!("{name} approved"))
            }
            _ => {
                return self.say(format!(
                    "/flow {verb}? run, pause, resume, approve, edit, new, test, park, unpark or reload"
                ));
            }
        };
        match done {
            Ok(said) => {
                self.say(said);
                self.refresh().await;
            }
            Err(e) => self.say(format!("{verb} {name}: {e:#}")),
        }
    }

    /// `^x` twice on a flow in the overview, and `/flow park`: the flow put away, its folder
    /// `_<name>`, its runs archived and its yes taken. One firing, or waiting on a question, is
    /// not: the daemon says why, and that is said as it is.
    pub(super) async fn park_flow(&mut self, name: &str) {
        let key = rows::flow_key(name);
        // On it in the overview, where the cursor goes once it is gone: the next flow down, else
        // `+ new flow`. Its page, if up, is closed back to the overview.
        let (next, its_page) = if self.view == View::Overview && self.sel.as_deref() == Some(key.as_str()) {
            let keys = self.keys();
            let next = keys
                .iter()
                .skip_while(|k| **k != key)
                .skip(1)
                .find(|k| k.starts_with("f:"))
                .cloned()
                .unwrap_or_else(|| NEW_FLOW.to_string());
            let page = self.show_pane && self.flow_proposal_shown().is_none() && self.proposal_shown().is_none();
            (Some(next), page)
        } else {
            (None, false)
        };
        match self.rpc.call::<FlowPark>(FlowName { name: name.into() }).await {
            Ok(p) => {
                if its_page {
                    self.leave_subpage();
                }
                // The refresh fetches the parked flows again, the overview's `parked: N` with them.
                self.refresh().await;
                if let Some(next) = next.filter(|k| self.keys().contains(k)) {
                    self.select(Some(next));
                    self.load_pane().await;
                }
                let runs = if p.archived > 0 { " · its runs archived" } else { "" };
                self.say(format!("{name} parked{runs} · /flow unpark {name} brings it back"));
            }
            Err(e) => self.say(told(&e)),
        }
    }

    /// `/flow unpark <name|folder>`: a folder (`_nightly.2`) is brought back as it is; a name, its
    /// newest parked folder, from the list as it is now.
    async fn unpark_named(&mut self, what: &str) {
        if !what.starts_with('_') {
            self.fetch_parked().await;
        }
        match crate::flow::parked_folder(&self.parked, what) {
            Some(folder) => self.unpark(&folder).await,
            None => self.say(format!("nothing parked as {what} · rl flow parked lists them")),
        }
    }

    /// `enter` on a parked flow, and `/flow unpark`: the folder back under the flow's name,
    /// needing a yes, and the cursor on it in the overview. One whose `in` is another project is
    /// not here to land on: the flash says where it went.
    pub(super) async fn unpark(&mut self, folder: &str) {
        let back = self
            .rpc
            .call::<FlowUnpark>(ParkedName {
                folder: folder.to_string(),
            })
            .await;
        // What is parked and what is loaded are read again whatever came of it, and said last: a
        // refresh that fails says so, and this is what matters.
        match back {
            Ok(Unparked { flow: Some(flow), .. }) => {
                self.refresh().await;
                let said = format!("{} back · it needs a yes: enter on it", flow.name);
                let said = self.onto_saved(&flow.name, said);
                self.say(said);
            }
            Ok(Unparked { name, problem, .. }) => {
                self.refresh().await;
                let why = problem.as_deref().unwrap_or("not loaded");
                self.say(format!(
                    "{name} is back in flows/{name} but did not load: {why} · ^e on it, or /flow reload"
                ));
            }
            Err(e) => {
                self.refresh().await;
                self.say(told(&e));
            }
        }
    }

    /// `/flow edit <name>`: its `flow.md` in `$EDITOR` once the screen is
    /// free — a new one from the bare template, one that did not load at the line
    /// where it broke.
    pub(super) fn edit_flow(&mut self, name: &str) {
        match crate::flow::start_edit(&self.home, name) {
            Ok(editing) => {
                let line = self
                    .not_loaded
                    .iter()
                    .find(|n| n.flow == name && n.file == editing.file)
                    .and_then(|n| n.line)
                    .unwrap_or(1);
                self.editor = Some((editing, line));
            }
            Err(e) => self.say(format!("{e:#}")),
        }
    }

    /// Back from the editor: every flow read again, and the one edited said
    /// yes to only as `rl flow edit` gives it — saved, loaded, and no more
    /// than the edit in it; else why not. A new flow left as the template is taken away first; a
    /// change proposed in the pane is the proposal's to settle (`edited_change`).
    pub(super) async fn edited(&mut self, editing: crate::flow::Editing) {
        if let Some(p) = self.editing_change.take() {
            return self.edited_change(editing, p).await;
        }
        let name = editing.name.clone();
        let said = match crate::flow::drop_new(&self.home, &editing, false) {
            Ok(true) => Ok(crate::flow::Edited::Dropped),
            Ok(false) => crate::flow::finish_edit(&mut self.rpc, editing).await.map(|(e, _)| e),
            Err(e) => Err(e),
        };
        match said {
            Ok(edited) => self.say(edited.words(&name)),
            Err(e) => self.say(format!("edit {name}: {e:#}")),
        }
        self.refresh().await;
    }

    /// `/flow …`: `reload` reads every flow again; the rest act on the one
    /// named after the verb, else on the flow under the cursor.
    pub(super) async fn flow_command(&mut self, arg: &str) {
        let mut words = arg.split_whitespace();
        let verb = words.next().unwrap_or("");
        if verb == "new" {
            let rest: Vec<&str> = words.collect();
            return self.new_flow(&rest);
        }
        let named = words.next().map(str::to_string);
        if verb == "reload" {
            return self.reload_flows().await;
        }
        if verb == "draft" {
            return self.show_flow_proposal();
        }
        if verb == "unpark" {
            let Some(what) = named.or_else(|| self.parked_here()) else {
                return self.say("/flow unpark which? name it: /flow unpark <flow> · its folder for an older one");
            };
            return self.unpark_named(&what).await;
        }
        if !matches!(verb, "run" | "pause" | "resume" | "approve" | "edit" | "test" | "park") {
            return self.say("/flow run|pause|resume|approve|edit|new|test|draft|park|unpark|reload [name]");
        }
        let Some(name) = named.or_else(|| self.flow_here()) else {
            return self.say(format!("/flow {verb} which? name it: /flow {verb} <flow>"));
        };
        match verb {
            "edit" => self.edit_flow(&name),
            "test" => self.test_flow(name).await,
            "park" => self.park_flow(&name).await,
            _ => self.flow_act(verb, name).await,
        }
    }

    /// `/reload` and `/flow reload`: every flow read again, and what did not
    /// load said — one in the flash by name, more by count with the pane
    /// listing each with its file and line.
    pub(super) async fn reload_flows(&mut self) {
        match self.rpc.call::<FlowReload>(()).await {
            Ok(r) => {
                self.say(flows::reload_note(&r));
                if r.not_loaded.len() > 1 {
                    self.pane = Pane::Facts;
                    self.cover_proposals();
                    self.show_pane = true;
                    self.reload_list = Some(r.not_loaded);
                }
                self.refresh().await;
            }
            Err(e) => self.say(format!("reload: {e:#}")),
        }
    }

    /// What did not load of the flows `in` one folder, as they were last
    /// loaded; or (`None`) the rest — those with no `in`, or never loaded.
    pub(super) fn broken_in(&self, scope: Option<&Path>) -> Vec<&NotLoaded> {
        self.not_loaded
            .iter()
            .filter(|n| {
                let was = self
                    .flows
                    .iter()
                    .find(|f| f.name == n.flow)
                    .and_then(|f| f.folder.as_deref());
                was == scope
            })
            .collect()
    }

    /// Whether a header's flows — a folder's, or `everywhere`'s — did not
    /// all load.
    pub(super) fn header_broken(&self, h: &rows::Head) -> bool {
        let scope = (!h.is_everywhere()).then_some(h.workspace.as_path());
        !self.broken_in(scope).is_empty()
    }

    /// What opens a header's first flow that did not load, as a hint:
    /// `/flow edit <name> · `; nothing when they all loaded.
    pub(super) fn broken_hint(&self, h: &rows::Head) -> String {
        let scope = (!h.is_everywhere()).then_some(h.workspace.as_path());
        self.broken_in(scope)
            .first()
            .map(|n| format!("/flow edit {} · ", n.flow))
            .unwrap_or_default()
    }
}

/// What the daemon said in refusing, as it said it; anything else as it came.
fn told(e: &anyhow::Error) -> String {
    match e.downcast_ref::<rl_proto::rpc::RpcError>() {
        Some(r) => r.message.clone(),
        None => format!("{e:#}"),
    }
}

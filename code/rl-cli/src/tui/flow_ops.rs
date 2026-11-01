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
                    "/flow {verb}? run, pause, resume, approve, edit, new, test or reload"
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
    /// than the edit in it; else why not. A new flow left as the template is taken away first.
    pub(super) async fn edited(&mut self, editing: crate::flow::Editing) {
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
        if !matches!(verb, "run" | "pause" | "resume" | "approve" | "edit" | "test") {
            return self.say("/flow run|pause|resume|approve|edit|new|test|draft|reload [name]");
        }
        let Some(name) = named.or_else(|| self.flow_here()) else {
            return self.say(format!("/flow {verb} which? name it: /flow {verb} <flow>"));
        };
        match verb {
            "edit" => self.edit_flow(&name),
            "test" => self.test_flow(name).await,
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

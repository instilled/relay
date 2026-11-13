//! What a row belongs to — its project, whether it comes onto the list, and whether you have it hidden.

use super::*;

/// Whether a row's kind lets it onto the list, given the toggles: an agent
/// when `show_agents`, a shell when `show_shells`, anything else always.
/// Whether it is hidden is `build`'s to say.
pub fn keeps(r: &Row, show_agents: bool, show_shells: bool) -> bool {
    match r.kind {
        RunKind::Agent => show_agents,
        RunKind::Shell => show_shells,
        RunKind::Command => true,
    }
}

/// The project a row stands under: the folder you named that its folder is
/// in, the deepest where they nest; else its repository's root (a linked
/// worktree is a root of its own), as read with the rows (`Row::repo`);
/// else its own folder.
pub fn project_of(r: &Row, names: &Names) -> PathBuf {
    names
        .containing(&r.cwd)
        .or_else(|| r.repo.clone())
        .unwrap_or_else(|| r.cwd.clone())
}

/// The folders you added as projects (`/project`).
pub fn added(prefs: &ListPrefs) -> impl Iterator<Item = PathBuf> + '_ {
    prefs.projects.iter().filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// The folders the list will have projects for: those its rows stand under
/// and those you named or added. What `git::checkouts` is asked about.
pub fn project_folders(rows: &[Row], prefs: &ListPrefs) -> Vec<PathBuf> {
    let names = Names::new(&prefs.names);
    let mut folders: Vec<PathBuf> = rows.iter().map(|r| project_of(r, &names)).collect();
    folders.extend(names.paths());
    folders.extend(added(prefs));
    folders.sort();
    folders.dedup();
    folders
}

/// A project's header key: `g:<folder>`, what hides it and orders it.
pub fn head_key(folder: &Path) -> String {
    format!("g:{}", folder.display())
}

/// What hides a row from the list — its own hide, or its project's — and
/// since when: the later of them. `None`: it is on the list.
pub fn hidden_since(r: &Row, project: &Path, hidden: &[Hidden]) -> Option<Timestamp> {
    let keys = [r.key.clone(), head_key(project)];
    hidden.iter().filter(|h| keys.contains(&h.key)).map(|h| h.at).max()
}

/// Whether what hides a row is its project rather than its own hide — what
/// bringing the row back alone cannot undo.
pub fn hidden_with(project: &Path, hidden: &[Hidden]) -> Option<&'static str> {
    let key = head_key(project);
    hidden.iter().any(|h| h.key == key).then_some("its project")
}

/// How many hidden rows — by their own hide or their project's — need you:
/// the count the header's hidden toggle carries while it is off.
pub fn hidden_wanting(rows: &[Row], prefs: &ListPrefs) -> usize {
    let names = Names::new(&prefs.names);
    rows.iter()
        .filter(|r| wants(r.state) && hidden_since(r, &project_of(r, &names), &prefs.hidden).is_some())
        .count()
}

/// Whether a row wants you: it asks, is blocked, or failed.
pub fn wants(s: RunState) -> bool {
    matches!(s, RunState::NeedsInput | RunState::Blocked | RunState::Failed)
}

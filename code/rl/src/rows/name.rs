//! Folders and rows by the names you gave them, and paths read short.

use super::*;

/// Your names: a project's by `g:<folder>`, a row's by its key.
#[derive(Clone, Copy)]
pub struct Names<'a>(&'a BTreeMap<String, String>);

impl<'a> Names<'a> {
    pub fn new(names: &'a BTreeMap<String, String>) -> Self {
        Self(names)
    }

    /// Every folder you named, by path.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.0
            .keys()
            .filter_map(|k| k.strip_prefix("g:"))
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    /// The named folder `p` is in, or is: the deepest where they nest.
    pub fn containing(&self, p: &Path) -> Option<PathBuf> {
        self.paths()
            .into_iter()
            .filter(|w| p.starts_with(w))
            .max_by_key(|w| w.as_os_str().len())
    }

    /// The name you gave a folder, if you did.
    pub fn registered(&self, p: &Path) -> Option<String> {
        self.0.get(&head_key(p)).cloned()
    }

    /// A folder by your name for it, else its last path component — except
    /// your home folder, which is `home`, not your login.
    pub fn workspace(&self, p: &Path) -> String {
        if let Some(n) = self.registered(p) {
            return n;
        }
        if std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == p) {
            return "home".into();
        }
        p.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Your name for a folder, where it differs from the folder's own.
    pub fn own(&self, p: &Path) -> Option<String> {
        let name = self.registered(p)?;
        let last = p.file_name().map(|s| s.to_string_lossy().into_owned());
        (last.as_deref() != Some(name.as_str())).then_some(name)
    }

    /// What a row is called: your name for it, else its own.
    pub fn title<'r>(&self, r: &'r Row) -> &'r str
    where
        'a: 'r,
    {
        self.0.get(&r.key).map_or(r.name.as_str(), String::as_str)
    }
}

/// A path with `~` for home — only for home itself and what is under it, not
/// a sibling that happens to share its start.
pub fn short_path(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) if !h.as_os_str().is_empty() => match p.strip_prefix(&h) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.to_string_lossy()),
            Err(_) => p.to_string_lossy().into_owned(),
        },
        _ => p.to_string_lossy().into_owned(),
    }
}

/// Where a shell is, as its row says it: inside its project, the way down
/// from the project's folder (`app/src`); at the project's folder, nothing;
/// anywhere else, the whole path with `~` for home.
pub fn shell_place(cwd: &Path, project: &Path) -> String {
    match cwd.strip_prefix(project) {
        Ok(rest) => rest.to_string_lossy().into_owned(),
        Err(_) => short_path(cwd),
    }
}

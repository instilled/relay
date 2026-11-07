//! `impl App` — the cursor: which line it is on, moving it, and where a
//! search puts it.

use super::app::App;
use crate::query;
use crate::rows::{self, Line, Names};

impl App {
    /// Keys of the list's lines, in order: every header and every row.
    pub fn keys(&self) -> Vec<String> {
        self.lines().iter().map(Line::key).collect()
    }

    /// Keeps the cursor on something that is there: the first line, when it
    /// has to move.
    pub fn settle(&mut self) {
        let keys = self.keys();
        if self.sel.as_ref().is_none_or(|k| !keys.contains(k)) {
            self.sel = keys.into_iter().next();
        }
    }

    pub fn select(&mut self, key: Option<String>) {
        self.sel = key;
    }

    /// The line under the cursor.
    pub fn selected(&self) -> Option<Line<'_>> {
        let key = self.sel.as_ref()?;
        self.lines().into_iter().find(|l| l.key() == *key)
    }

    /// The cursor `by` lines on (back, when negative), stopping at the ends.
    pub fn step(&mut self, by: isize) {
        let keys = self.keys();
        if keys.is_empty() {
            return;
        }
        let cur = self
            .sel
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
            .unwrap_or(0) as isize;
        let next = cur.saturating_add(by).clamp(0, keys.len() as isize - 1) as usize;
        self.select(Some(keys[next].clone()));
    }

    /// Keys of what the search's words name, best first; see `query::ranked`.
    /// Nothing when it has no words, only tokens.
    pub fn matches(&self) -> Vec<String> {
        let words = query::Query::parse(&self.filter).words;
        if words.is_empty() {
            return vec![];
        }
        let names = Names::new(&self.config.list.names);
        let lines = self.lines();
        let named: Vec<query::Named> = lines.iter().map(|l| rows::named(l, &names)).collect();
        query::ranked(&words, &named)
            .into_iter()
            .map(|i| lines[i].key())
            .collect()
    }

    /// The search changed: the cursor goes to the best match. With no words
    /// to match, it stays where it is if it can.
    pub fn land(&mut self) {
        match self.matches().into_iter().next() {
            Some(k) => self.select(Some(k)),
            None => self.settle(),
        }
    }

    /// `tab` with nothing to complete: on to the next match, round;
    /// shift-tab back. False when there is none to go to.
    pub fn next_match(&mut self, back: bool) -> bool {
        let order = self.matches();
        let Some(k) = query::next_match(&order, self.sel.as_ref(), back).cloned() else {
            return false;
        };
        self.select(Some(k));
        true
    }
}

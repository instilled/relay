//! `impl App` — the cursor: which line it is on, moving it, and where a
//! search puts it.

use super::app::App;
use crate::query;
use crate::rows::{self, Line, Names};

impl App {
    /// Keeps the cursor on something that is there: the first session, when
    /// it has to move (a header only if there is no session), so `enter`
    /// does something on open.
    pub fn settle(&mut self) {
        if self.selected().is_some() {
            return;
        }
        let lines = self.lines();
        let first = lines
            .iter()
            .find(|l| l.row().is_some())
            .or(lines.first())
            .map(Line::key);
        self.sel = first;
    }

    /// Where the cursor goes once the line it is on has left, best first:
    /// the next row of the same project, else the one before it there, else
    /// the project's header; only then the line after, else the one before.
    /// So removing a project's last row leaves the cursor in that project.
    pub fn near_keys(&self) -> Vec<String> {
        let lines = self.lines();
        let Some(at) = self.sel.as_ref().and_then(|k| lines.iter().position(|l| l.key() == *k)) else {
            return Vec::new();
        };
        let mut near = Vec::new();
        if let Some(l) = lines[at + 1..].iter().take_while(|l| l.row().is_some()).next() {
            near.push(l.key());
        }
        let before = lines[..at].iter().rev().take_while(|l| l.row().is_some());
        if let Some(l) = before.clone().next() {
            near.push(l.key());
        }
        if let Some(head) = lines[..at].iter().rev().find(|l| l.row().is_none()) {
            near.push(head.key());
        }
        near.extend(lines.get(at + 1).map(Line::key));
        near.extend(at.checked_sub(1).and_then(|i| lines.get(i)).map(Line::key));
        near
    }

    /// The cursor on the first of `near` still on the list, else settled.
    pub fn select_near(&mut self, near: &[String]) {
        let keys = self.keys();
        self.sel = near.iter().find(|k| keys.contains(k)).cloned();
        self.settle();
    }

    pub fn select(&mut self, key: Option<String>) {
        if key != self.sel {
            self.moved = std::time::Instant::now();
        }
        self.sel = key;
    }

    /// The line under the cursor.
    pub fn selected(&self) -> Option<Line<'_>> {
        self.line_keyed(self.sel.as_ref()?)
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

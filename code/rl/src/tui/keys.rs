//! `impl App` — what the keys, the mouse and a paste do on the list.

use super::app::{App, Arm, Search};
use super::draw::hit_row;
use super::{line, shortcuts};
use crate::query;
use crate::rows::{self, Names};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// `^j` and `^k` are `↓` and `↑`, as in vi.
pub fn vi_step(k: KeyEvent) -> KeyEvent {
    if k.modifiers != KeyModifiers::CONTROL {
        return k;
    }
    match k.code {
        KeyCode::Char('j') => KeyEvent::from(KeyCode::Down),
        KeyCode::Char('k') => KeyEvent::from(KeyCode::Up),
        _ => k,
    }
}

impl App {
    pub fn key(&mut self, k: KeyEvent) {
        self.flash = None;
        // A key armed by its first press stays armed only until the very next
        // key: its own second press acts, anything else disarms it.
        let armed = self.arm.take();
        if (k.code, k.modifiers) == (KeyCode::Char('f'), KeyModifiers::CONTROL) {
            match self.search.take() {
                Some(_) => {}
                None => self.open_search(),
            }
            return;
        }
        if self.search.is_some() {
            self.key_search(k, armed);
            return;
        }
        match shortcuts::take(self.shortcuts, &k) {
            shortcuts::Take::Toggle => {
                self.shortcuts = !self.shortcuts;
                return;
            }
            shortcuts::Take::Close => {
                self.shortcuts = false;
                return;
            }
            shortcuts::Take::Pass => {}
        }
        if k.modifiers.is_empty() {
            match k.code {
                KeyCode::Char('a') => return self.toggle_agents(),
                KeyCode::Char('s') => return self.toggle_shells(),
                KeyCode::Char('h') => return self.toggle_hidden(),
                _ => {}
            }
        }
        if (k.code, k.modifiers) == (KeyCode::Char('s'), KeyModifiers::CONTROL) {
            return self.toggle_grouping();
        }
        self.key_list(vi_step(k), armed);
    }

    /// `^f`: search, starting from the filter already in force.
    pub fn open_search(&mut self) {
        // What is typed now narrows the list: the shortcuts would only be in
        // the way.
        self.shortcuts = false;
        let mut l = line::Line::default();
        l.set_with_cursor(self.filter.clone(), self.filter.len());
        self.search = Some(Search {
            line: l,
            before: self.filter.clone(),
        });
    }

    /// A key while searching: typing narrows at once and puts the cursor on
    /// the best match, `enter` keeps the search, `esc` puts back the filter
    /// from before, `tab` completes a token or else goes to the next match;
    /// the arrows still move through what is left.
    fn key_search(&mut self, k: KeyEvent, armed: Option<Arm>) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Enter || (ctrl && k.code == KeyCode::Char('j')) {
            self.search = None;
            return;
        }
        if k.code == KeyCode::BackTab {
            self.next_match(true);
            return;
        }
        match s.line.key(&k) {
            line::Act::Typing => {
                let now = s.line.text().trim().to_string();
                if now != self.filter {
                    self.filter = now;
                    self.land();
                }
            }
            line::Act::Cancel => {
                if let Some(s) = self.search.take() {
                    self.filter = s.before;
                    self.settle();
                }
            }
            line::Act::Complete => self.complete_search(),
            line::Act::Submit => self.search = None,
            line::Act::Nothing => self.key_list(vi_step(k), armed),
        }
    }

    /// `tab` in a search: the token names, then a token's values — states,
    /// kinds, projects, branches. One fits: taken. Several: what they share,
    /// and the rest shown in the footer. None — a plain word, or no word at
    /// all after one — and it goes to the next match instead.
    fn complete_search(&mut self) {
        let names = Names::new(&self.config.list.names);
        let mut projects: Vec<String> = self
            .lines_with("")
            .iter()
            .filter_map(|l| match l {
                rows::Line::Group(h) if h.is_project() => Some(h.name.clone()),
                _ => None,
            })
            .chain(names.paths().iter().map(|p| names.workspace(p)))
            .collect();
        projects.sort();
        projects.dedup();
        let branches: Vec<String> = self.checkouts.values().filter_map(|c| c.branch.clone()).collect();
        let kinds = rows::list_kinds();
        let Some(s) = self.search.as_ref() else {
            return;
        };
        let (text, cur) = (s.line.text().to_string(), s.line.cursor());
        let start = text[..cur].rfind(' ').map_or(0, |i| i + 1);
        let word = &text[start..cur];
        let cands = query::complete(word, &projects, &branches, &kinds);
        if (cands.is_empty() || word.is_empty()) && self.next_match(false) {
            return;
        }
        let take = match cands.as_slice() {
            [] => return,
            [one] if one.ends_with(':') => one.clone(),
            [one] => format!("{one} "),
            many => {
                let mut prefix = many[0].clone();
                for c in &many[1..] {
                    while !c.starts_with(&prefix) {
                        prefix.pop();
                    }
                }
                if prefix.len() <= word.len() {
                    self.say(many.join(" · "));
                    return;
                }
                prefix
            }
        };
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let line = format!("{}{take}{}", &text[..start], &text[cur..]);
        s.line.set_with_cursor(line, start + take.len());
        self.filter = s.line.text().trim().to_string();
        self.land();
    }

    /// What is not typed: moving about, and backing out.
    fn key_list(&mut self, k: KeyEvent, armed: Option<Arm>) {
        let page = self.list_h.get().saturating_sub(2).max(1) as isize;
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if armed == Some(Arm::Quit) {
                    self.quit = true;
                } else {
                    self.arm = Some(Arm::Quit);
                    self.say("press ^c again to quit");
                }
            }
            (KeyCode::Esc, _) => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.settle();
                }
            }
            (KeyCode::Down, _) => self.step(1),
            (KeyCode::Up, _) => self.step(-1),
            (KeyCode::Home, _) => self.step(isize::MIN),
            (KeyCode::End, _) => self.step(isize::MAX),
            (KeyCode::PageDown, _) => self.step(page),
            (KeyCode::PageUp, _) => self.step(-page),
            _ => {}
        }
    }

    /// The wheel steps the cursor — the list scrolls to follow it — and the
    /// cursor travels with the pointer: moving over a line puts it there, and
    /// so does a click. All only over the list itself.
    pub fn mouse(&mut self, m: MouseEvent) {
        if !self.hit.borrow().has(m.column, m.row) {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.step(1),
            MouseEventKind::ScrollUp => self.step(-1),
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                // Bound first: the borrow has to be done with before `select`.
                let key = hit_row(&self.hit.borrow(), m.column, m.row);
                if key.is_some() {
                    self.select(key);
                }
            }
            _ => {}
        }
    }

    /// A paste goes into the search, as one line; elsewhere nothing takes it.
    pub fn paste(&mut self, s: &str) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let s: String = s
            .chars()
            .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        search.line.insert(&s);
        let now = search.line.text().trim().to_string();
        if now != self.filter {
            self.filter = now;
            self.land();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_j_and_k_move() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(vi_step(ctrl('j')).code, KeyCode::Down);
        assert_eq!(vi_step(ctrl('k')).code, KeyCode::Up);
        // Plain j and k are letters, and other ctrl keys are left alone.
        let j = KeyEvent::from(KeyCode::Char('j'));
        assert_eq!(vi_step(j), j);
        assert_eq!(vi_step(ctrl('x')), ctrl('x'));
    }
}

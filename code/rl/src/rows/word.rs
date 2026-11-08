//! A row in words: its icon and colour, how it stands, and the little phrasings and columns around it.

use super::*;

/// A state as a word on a Relay row, with `~` or `?` when it is a guess.
pub fn state_words(state: RunState, confidence: Confidence) -> String {
    let word = match state {
        RunState::Starting => "starting",
        RunState::Working => "working",
        RunState::Idle => "idle",
        RunState::NeedsInput => "needs you",
        RunState::Blocked => "blocked",
        RunState::Stale => "stale",
        RunState::Finished => "done",
        RunState::Failed => "failed",
        RunState::Stopped => "stopped",
    };
    match confidence {
        Confidence::Exact => word.to_string(),
        Confidence::Likely => format!("{word}~"),
        Confidence::Guess => format!("{word}?"),
    }
}

/// What a search's `is:` takes a state as.
pub fn is_word(s: RunState) -> &'static str {
    match s {
        RunState::Failed => "failed",
        RunState::NeedsInput | RunState::Blocked => "needs",
        RunState::Finished => "done",
        RunState::Stopped => "stopped",
        RunState::Starting | RunState::Working | RunState::Stale | RunState::Idle => "going",
    }
}

/// How a harness's rows look: its glyph and its colour, as its profile has them.
pub struct Look {
    pub icon: Option<String>,
    pub color: Option<(u8, u8, u8)>,
}

/// The profiles: the built-ins with yours in `$RELAY_HOME/detect/` over
/// them, read once (the built-ins alone under test). Profiles that do not
/// load leave the built-ins.
pub fn registry() -> &'static rl_detect::Registry {
    use rl_detect::Registry;
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        if cfg!(test) {
            Registry::builtin()
        } else {
            rl_core::Home::resolve()
                .ok()
                .and_then(|h| Registry::load(&h).ok())
                .unwrap_or_else(Registry::builtin)
        }
    })
}

/// What `kind:` takes on the list: `query::KINDS`, and each agent's name
/// (`kind:codex`: its sessions, wherever they are).
pub fn list_kinds() -> Vec<String> {
    crate::query::KINDS
        .iter()
        .map(|k| k.to_string())
        .chain(registry().agents().map(|p| p.name.clone()))
        .collect()
}

/// Every harness's look, by name, as its profile (`registry`) has it.
pub fn looks() -> &'static BTreeMap<String, Look> {
    static LOOKS: OnceLock<BTreeMap<String, Look>> = OnceLock::new();
    LOOKS.get_or_init(|| {
        registry()
            .agents()
            .map(|p| {
                let look = Look {
                    icon: p.icon.clone(),
                    color: p.color,
                };
                (p.name.clone(), look)
            })
            .collect()
    })
}

pub fn is_shell(r: &Row) -> bool {
    r.kind == RunKind::Shell
}

/// A shell that is simply there to be typed in. One that ended, failed or
/// wants you is news like any other row and is left to read as one.
pub fn live_shell(r: &Row) -> bool {
    is_shell(r) && !r.state.is_terminal() && !matches!(r.state, RunState::NeedsInput | RunState::Blocked)
}

/// A row's icon: an agent — in its harness's own glyph, where its profile
/// has one — a shell, or a command.
pub fn icon(r: &Row) -> &'static str {
    match r.kind {
        RunKind::Agent => looks().get(&r.harness).and_then(|l| l.icon.as_deref()).unwrap_or(AGENT),
        RunKind::Shell => SHELL,
        RunKind::Command => COMMAND,
    }
}

/// A header's place in the columns it has: the folder above, dim; the folder
/// itself, bold; and, when it goes by a name of its own, that name — ` · api`.
/// A path too long for the room loses its start to `…`, a whole folder at a
/// time where it can; the folder itself is never cut. `(parent, last, own)`.
pub fn place_parts(place: &str, own: Option<&str>, room: usize) -> (String, String, String) {
    let (parent, last) = match place.rfind('/') {
        Some(i) if i + 1 < place.len() => (&place[..=i], &place[i + 1..]),
        _ => ("", place),
    };
    let own = own.map(|n| format!(" · {n}")).unwrap_or_default();
    let n = |s: &str| s.chars().count();
    let need = n(last) + n(&own);
    if n(parent) + need <= room {
        return (parent.to_string(), last.to_string(), own);
    }
    // The parent gives way first, from the left.
    if need + 2 <= room {
        let keep = room - need - 1;
        let tail: String = parent.chars().skip(n(parent) - keep).collect();
        let tail = match tail.find('/') {
            Some(i) if i > 0 => tail[i..].to_string(),
            _ => tail,
        };
        return (format!("…{tail}"), last.to_string(), own);
    }
    // Then the name; the folder stands whole, whatever the room.
    let own_room = room.saturating_sub(n(last));
    let own = if own_room >= 5 {
        fit(&own, own_room).trim_end().to_string()
    } else {
        String::new()
    };
    (String::new(), last.to_string(), own)
}

/// `s` in exactly `w` columns as a terminal shows it: padded, or cut with
/// `…`. Columns, not chars: a wide glyph (CJK, most emoji) takes two, and
/// one that would straddle the cut gives way to a space.
pub fn fit(s: &str, w: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let n = s.width();
    if n <= w {
        return format!("{s}{}", " ".repeat(w - n));
    }
    if w == 0 {
        return String::new();
    }
    let mut t = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w - 1 {
            break;
        }
        used += cw;
        t.push(c);
    }
    t.push('…');
    t.push_str(&" ".repeat(w - 1 - used));
    t
}

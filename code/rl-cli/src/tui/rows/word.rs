//! A row in words: its icon and colour, what it is and how it stands, where it is from, and the little phrasings around it.

use super::*;

/// A checkout on a header: `⎇ main · PR #95 · 3 worktrees` — the badges after
/// the branch, the count on the main checkout only and only when there are any.
pub fn checkout_words(c: &Checkout) -> String {
    let mut words = vec![format!("⎇ {}", c.head())];
    words.extend(c.badges.iter().cloned());
    match c.worktrees {
        0 => {}
        1 => words.push("1 worktree".into()),
        n => words.push(format!("{n} worktrees")),
    }
    words.join(" · ")
}

/// What a run's row says of its branch: only where it started on another
/// than the one its folder is on now, so a switch does not hide where it ran.
pub fn branch_aside(r: &Run, checkouts: &[Checkout]) -> Option<String> {
    let started = r.branch.as_deref()?;
    let now = checkout_of(checkouts, &r.workspace)?;
    (now.branch.as_deref() != Some(started)).then(|| format!("⎇ {started}"))
}

/// A folder's checkout among those the digest carries.
pub fn checkout_of<'a>(checkouts: &'a [Checkout], folder: &Path) -> Option<&'a Checkout> {
    checkouts.iter().find(|c| c.path == folder)
}

/// A run's state as a word on its row, with `~` or `?` when it is a guess.
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

/// An icon's colour: a shell the prompt's green, a question amber, a command
/// and a flow quiet, an agent its CLI's colour (`cli`) or the plain
/// foreground. Anything that has ended is quiet too — its word says how it
/// ended.
pub fn icon_style(icon: &str, cli: Option<&str>, ended: bool) -> ratatui::style::Style {
    use crate::tui::theme;
    use ratatui::style::{Color, Style};
    if ended {
        return theme::dim();
    }
    match icon {
        SHELL => theme::accent(),
        QUESTION => theme::needs(),
        COMMAND | FLOW => theme::dim(),
        _ => match cli.and_then(|c| looks().get(c)).and_then(|l| l.color) {
            Some((r, g, b)) => Style::default().fg(Color::Rgb(r, g, b)),
            None => Style::default(),
        },
    }
}

/// How a CLI's rows look: its glyph and its colour, as its profile has them.
pub(crate) struct Look {
    pub(crate) icon: Option<String>,
    pub(crate) color: Option<(u8, u8, u8)>,
}

/// The profiles: the built-ins with yours in `$RELAY_HOME/detect/` over
/// them, read once (the built-ins alone under test). Profiles that do not
/// load leave the built-ins — `rld` is the one that says what is wrong
/// with them.
pub(crate) fn registry() -> &'static rl_detect::Registry {
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
/// (`kind:codex`: its runs and its own sessions).
pub fn list_kinds() -> Vec<String> {
    crate::tui::query::KINDS
        .iter()
        .map(|k| k.to_string())
        .chain(registry().agents().map(|p| p.name.clone()))
        .collect()
}

/// Every CLI's look, by name, as its profile (`registry`) has it.
pub(crate) fn looks() -> &'static BTreeMap<String, Look> {
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

/// A shell: an interactive one by its profile, or a project's warm shell by
/// its tag, whatever its argv resolved to.
pub fn is_shell(r: &Run) -> bool {
    r.is_shell()
}

/// A shell that is simply there to be typed in. One that ended, failed or
/// wants you is news like any other run and is left to read as one.
pub fn live_shell(r: &Run) -> bool {
    is_shell(r) && !r.state.is_terminal() && !matches!(r.state, RunState::NeedsInput | RunState::Blocked)
}

/// What a run's row says in its summary column. A Claude or Codex run that is
/// waiting — on you, or for nothing, or ended — says its latest recap, which
/// is what you come back for; at work, what it is doing.
pub fn said(i: &DigestItem) -> &str {
    match &i.recap {
        Some(r) if !matches!(i.run.state, RunState::Starting | RunState::Working) => r,
        _ => &i.summary,
    }
}

/// A run's icon: an agent — in its CLI's own glyph, where its profile has
/// one — a shell, or a command (a plugin is one).
pub fn icon(r: &Run) -> &'static str {
    if is_shell(r) {
        return SHELL;
    }
    match r.kind {
        RunKind::Agent => r
            .cli
            .as_deref()
            .and_then(|c| looks().get(c)?.icon.as_deref())
            .unwrap_or(AGENT),
        RunKind::Shell => SHELL,
        RunKind::Command | RunKind::Plugin => COMMAND,
    }
}

/// The icon of one of an agent's own sessions: its agent's glyph, as its
/// runs wear it.
pub fn outside_icon(c: &Outside) -> &'static str {
    looks().get(&c.agent).and_then(|l| l.icon.as_deref()).unwrap_or(AGENT)
}

/// Whether one of an agent's own sessions has ended, so its icon is quiet.
pub fn outside_ended(c: &Outside) -> bool {
    c.run_state().is_terminal()
}

/// The word in the state column. A shell at rest says nothing there — it is
/// idle whenever you are not typing, and its icon already says what it is.
pub fn run_words(r: &Run) -> String {
    if live_shell(r) {
        String::new()
    } else {
        state_words(r.state, r.confidence)
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

/// Why a run started, in words.
pub fn cause_words(c: &Cause) -> String {
    match c {
        Cause::Manual => "started by hand".into(),
        Cause::Decision(_) => "your answer".into(),
        Cause::Flow { flow, step } => format!("flow {flow} · {step}"),
        Cause::Assistant(job) => format!("asked, to {job}"),
    }
}

/// The firing a question is a flow's ask in: the one whose id is its chain.
pub fn firing_of<'a>(d: &Decision, firings: &'a [Firing]) -> Option<&'a Firing> {
    firings.iter().find(|f| f.id == d.chain)
}

/// Who asked a question, in words: the run that asked; else the flow whose
/// firing it is, and the step that asks (`flow prod-logs · review`); else you.
pub fn asked_by(d: &Decision, run: Option<&Run>, firing: Option<&Firing>) -> String {
    if let Some(r) = run {
        return r.title.clone();
    }
    let Some(f) = firing else {
        return "you".into();
    };
    let step = f
        .steps
        .iter()
        .find(|s| s.decision.as_deref() == Some(d.id.as_str()))
        .or_else(|| f.steps.iter().find(|s| s.status == rl_proto::flow::StepStatus::Waiting));
    match step {
        Some(s) => format!("flow {} · {}", f.flow, s.name),
        None => format!("flow {}", f.flow),
    }
}

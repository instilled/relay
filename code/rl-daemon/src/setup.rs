//! What the setup guide settles, written into `config.yaml` — which stays
//! yours: a key setup sets replaces that key's block and nothing else, so
//! the comments and every other key are left as they were. How far the
//! guide got is kept in the ledger's `meta`, not the file.

use anyhow::{Context, Result, bail};
use rl_core::Home;
use rl_proto::config::{Assistant, Config};

/// The `meta` key: how many of the guide's steps were gone through.
pub const SEEN: &str = "setup.seen";

/// `config.yaml` with the agents offered and the assistant set, as asked;
/// `None` leaves that key alone. A file not there yet starts from the seed.
pub fn write(home: &Home, agents: Option<&[String]>, assistant: Option<&Assistant>) -> Result<()> {
    let file = home.config_file();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::config::SEED.to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let text = edit(&text, agents, assistant)?;
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))
}

/// The edit itself, checked: what comes out must still be a config.
fn edit(text: &str, agents: Option<&[String]>, assistant: Option<&Assistant>) -> Result<String> {
    let mut text = text.to_string();
    if let Some(a) = agents {
        let block = if a.is_empty() {
            None
        } else {
            let names: Vec<String> = a.iter().map(|x| scalar(x)).collect();
            Some(format!("agents: [{}]\n", names.join(", ")))
        };
        text = set_key(&text, "agents", block.as_deref());
    }
    if let Some(a) = assistant {
        let one = assistant_yaml(a);
        text = set_key(
            &text,
            "assistants",
            Some(&format!("assistants:\n  interpret: {one}\n  summarise: {one}\n")),
        );
    }
    if !text.trim().is_empty()
        && let Err(e) = serde_norway::from_str::<Config>(&text)
    {
        bail!("config.yaml would not read back ({e}); left as it was");
    }
    Ok(text)
}

fn assistant_yaml(a: &Assistant) -> String {
    if a.model.is_none() && a.args.is_empty() {
        return scalar(&a.agent);
    }
    let mut parts = vec![format!("agent: {}", scalar(&a.agent))];
    if let Some(m) = &a.model {
        parts.push(format!("model: {}", scalar(m)));
    }
    if !a.args.is_empty() {
        let args: Vec<String> = a.args.iter().map(|x| scalar(x)).collect();
        parts.push(format!("args: [{}]", args.join(", ")));
    }
    format!("{{ {} }}", parts.join(", "))
}

/// A plain word as it is; anything else quoted, which YAML reads as JSON does.
/// A number or a yes/no would be read as one, so those are quoted too.
fn scalar(s: &str) -> String {
    let plain = s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
        && !["true", "false", "yes", "no", "on", "off", "null", "y", "n"].contains(&s.to_ascii_lowercase().as_str());
    if plain {
        s.to_string()
    } else {
        serde_json::to_string(s).expect("a string serialises")
    }
}

/// `text` with the top-level `key` block replaced by `block` (`None`
/// removes it), or `block` added at the end when the key is not set. A block
/// is the key's line and the indented lines under it; a commented-out key is
/// not the key.
fn set_key(text: &str, key: &str, block: Option<&str>) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let starts = |l: &str| {
        l.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    };
    let Some(at) = lines.iter().position(|l| starts(l)) else {
        let Some(block) = block else {
            return text.to_string();
        };
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
        return out;
    };
    let mut end = at + 1;
    while end < lines.len() && lines[end].starts_with([' ', '\t']) {
        end += 1;
    }
    let mut out: String = lines[..at].concat();
    if let Some(block) = block {
        out.push_str(block);
    }
    out.push_str(&lines[end..].concat());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_haiku() -> Assistant {
        Assistant {
            agent: "claude".into(),
            model: Some("haiku".into()),
            args: vec![],
        }
    }

    #[test]
    fn the_seed_keeps_its_comments_and_gains_the_keys() {
        let agents = vec!["claude".to_string(), "codex".to_string()];
        let out = edit(crate::config::SEED, Some(&agents), Some(&claude_haiku())).unwrap();
        assert!(out.starts_with(crate::config::SEED), "{out}");
        assert!(out.ends_with(
            "agents: [claude, codex]\nassistants:\n  interpret: { agent: claude, model: haiku }\n  summarise: { agent: claude, model: haiku }\n"
        ));
        let c: Config = serde_norway::from_str(&out).unwrap();
        assert_eq!(c.agents, agents);
        assert_eq!(c.assistants.summarise, Some(claude_haiku()));
    }

    #[test]
    fn a_key_already_there_is_replaced_in_place() {
        let text =
            "# mine\nassistants:\n  interpret: codex\n  summarise:\n    agent: codex\n    args: [x]\nretention: 30d\n";
        let out = edit(text, None, Some(&claude_haiku())).unwrap();
        assert_eq!(
            out,
            "# mine\nassistants:\n  interpret: { agent: claude, model: haiku }\n  summarise: { agent: claude, model: haiku }\nretention: 30d\n"
        );
        let out = edit("agents: [claude]\nretention: 30d\n", Some(&[]), None).unwrap();
        assert_eq!(out, "retention: 30d\n", "none picked: every agent, so the key goes");
        let out = edit("retention: 30d", Some(&["gemini".to_string()]), None).unwrap();
        assert_eq!(out, "retention: 30d\nagents: [gemini]\n");
    }

    #[test]
    fn nothing_asked_nothing_changed() {
        let text = "# only comments\n";
        assert_eq!(edit(text, None, None).unwrap(), text);
    }

    #[test]
    fn odd_values_are_quoted_and_still_read_back() {
        let a = Assistant {
            agent: "claude".into(),
            model: Some("a model: with colon".into()),
            args: vec!["--max-turns".into(), "1".into()],
        };
        let out = edit("", None, Some(&a)).unwrap();
        let c: Config = serde_norway::from_str(&out).unwrap();
        assert_eq!(c.assistants.interpret, Some(a));
    }

    #[test]
    fn a_file_that_would_not_read_back_is_refused() {
        let e = edit("retention: [\n", None, Some(&claude_haiku())).unwrap_err();
        assert!(e.to_string().contains("left as it was"), "{e}");
    }

    #[test]
    fn written_to_the_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        write(&home, Some(&["codex".to_string()]), None).unwrap();
        let l = crate::config::load(&home);
        assert!(l.error.is_none(), "{:?}", l.error);
        assert_eq!(l.config.agents, ["codex"]);
    }
}

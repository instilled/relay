//! Plain words → a draft: the context pack and the words go to
//! `assistants.interpret` as a run of that agent; the one YAML document it
//! answers with becomes the form, filled in, for the person's enter. Nothing
//! here acts on its own: `apply` is called only after that enter.

use crate::client::Client;
use crate::context::{self, Here};
use crate::tui::form::{self, Form};
use anyhow::{Context as _, Result, bail};
use rl_core::Home;
use rl_proto::methods::{AssistantOutput, AssistantParams, AssistantRun, RunSpec, RunStart};
use rl_proto::types::{Cause, Run};
use std::collections::BTreeMap;
use std::path::Path;

/// What a draft is. A run, for now: flows are written in `$EDITOR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
}

/// What the assistant answered, as the form's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub kind: Kind,
    pub fields: BTreeMap<String, String>,
}

impl Draft {
    pub fn get(&self, k: &str) -> &str {
        self.fields.get(k).map(String::as_str).unwrap_or("")
    }

    /// The draft back as YAML lines, for the line.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!(
            "kind: {}",
            match self.kind {
                Kind::Run => "run",
            }
        )];
        for key in ["in", "run", "title"] {
            if let Some(val) = self.fields.get(key)
                && !val.is_empty()
            {
                v.push(format!("{key}: {}", scalar(val)));
            }
        }
        v
    }
}

/// A value as YAML would read it back: quoted when it has to be.
fn scalar(s: &str) -> String {
    if s.contains(['#', ':', '"', '\'']) || s.starts_with(['-', ' ', '[', '{']) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// The draft in what the assistant said: the last block of `key: value`
/// lines that starts with `kind:`, fences and prose around it dropped. Read
/// by hand, not as YAML: an agent writes `run: claude fix it: now` and means
/// it, and a small fixed schema does not need a parser that would refuse it.
pub fn parse(raw: &str) -> Result<Draft> {
    // Blocks: runs of non-blank lines, fences and their contents' edges cut.
    let mut blocks: Vec<Vec<&str>> = vec![];
    let mut cur: Vec<&str> = vec![];
    for l in raw.lines() {
        let t = l.trim_end();
        let bare = t.trim_start();
        if bare.is_empty() || bare.starts_with("```") {
            if !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(t);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    let block = blocks
        .into_iter()
        .rev()
        .find(|b| b.iter().any(|l| l.trim_start().starts_with("kind:")))
        .ok_or_else(|| anyhow::anyhow!("no `kind:` in the answer"))?;
    let start = block.iter().position(|l| l.trim_start().starts_with("kind:")).unwrap();
    let indent = block[start].len() - block[start].trim_start().len();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut kind = None;
    let mut last_key: Option<String> = None;
    for l in &block[start..] {
        let lead = l.len() - l.trim_start().len();
        let t = l.trim();
        // A block list under the previous key: `- a`.
        if lead > indent
            && let Some(item) = t.strip_prefix("- ")
            && let Some(k) = &last_key
        {
            let v = fields.entry(k.clone()).or_default();
            if !v.is_empty() {
                v.push_str(", ");
            }
            v.push_str(&unquote(item));
            continue;
        }
        if lead != indent {
            break;
        }
        let Some((k, v)) = t.split_once(':') else { break };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        let v = strip_comment(v.trim());
        let v = if let Some(inner) = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            inner
                .split(',')
                .map(|x| unquote(x.trim()))
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            unquote(&v)
        };
        if k == "kind" {
            kind = Some(match v.as_str() {
                "run" => Kind::Run,
                other => bail!("kind: `{other}` is not run"),
            });
        } else {
            fields.insert(k.to_string(), v);
        }
        last_key = Some(k.to_string());
    }
    let kind = kind.ok_or_else(|| anyhow::anyhow!("no `kind:` in the answer"))?;
    Ok(Draft { kind, fields })
}

/// `# a comment` after a value goes; a `#` inside quotes or glued to a word
/// (`#123`) stays.
fn strip_comment(v: &str) -> String {
    if v.starts_with(['"', '\'']) {
        return v.to_string();
    }
    match v.find(" #") {
        Some(i) => v[..i].trim_end().to_string(),
        None => v.to_string(),
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        if let Some(inner) = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            return inner.replace("\\\"", "\"").replace("\\\\", "\\");
        }
        if let Some(inner) = v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
            return inner.replace("''", "'");
        }
    }
    v.to_string()
}

/// The run form for a draft, filled in.
pub fn form_of(d: &Draft, origin: &Path) -> Form {
    let mut f = Form::run(origin);
    for key in ["in", "run", "title"] {
        let v = d.get(key);
        if !v.is_empty() {
            f.set(key, v);
        }
    }
    f
}

/// Asks the interpreter: the pack for `folder`, then the words. What comes
/// back, parsed, with the run that said it.
pub async fn ask(home: &Home, folder: &Path, words: &str) -> Result<(Draft, AssistantOutput)> {
    let mut rpc = Client::connect(home).await?;
    let here: Here = context::gather(&mut rpc, folder).await?;
    let pack = context::render(context::help(), &here);
    let prompt = context::ask(&pack, words);
    let title = format!("interpret: {}", words.chars().take(48).collect::<String>());
    let out = rpc
        .call::<AssistantRun>(AssistantParams {
            job: "interpret".into(),
            prompt,
            cwd: folder.to_path_buf(),
            title,
            timeout: None,
        })
        .await?;
    if out.timed_out {
        bail!("{} took too long and was stopped (run {})", out.agent, out.run);
    }
    let draft = parse(&out.output)
        .with_context(|| format!("{} answered (run {}):\n{}", out.agent, out.run, out.output.trim()))?;
    Ok((draft, out))
}

/// What the form says, done: a run started. The person's yes, on the line.
/// Returns a line saying what happened.
pub async fn apply(home: &Home, f: &Form) -> Result<String> {
    let mut rpc = Client::connect(home).await?;
    let d = form::run_draft(f)?;
    let spec = RunSpec {
        argv: d.argv,
        kind: d.kind,
        cwd: d.cwd,
        title: d.title,
        ref_: None,
        tags: vec![],
        tty: true,
        cols: None,
        rows: None,
        env: crate::env::child_env(&[])?,
        inherit_env: false,
        input: true,
        cause: Cause::Manual,
        chain: None,
    };
    let run: Run = rpc.call::<RunStart>(spec).await?;
    Ok(format!("started {} · {}", run.title, run.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_found_among_fences_and_prose() {
        let raw = "Sure — here is a draft:\n```yaml\nkind: run\nin: ~/w/relay\nrun: make test\ntitle: tests\n```\nLet me know.";
        let d = parse(raw).unwrap();
        assert_eq!(d.kind, Kind::Run);
        assert_eq!(d.get("run"), "make test");
        assert_eq!(
            d.lines(),
            ["kind: run", "in: ~/w/relay", "run: make test", "title: tests"]
        );
        let d = parse("kind: run\nin: /w\nrun: claude fix the build: it's red   # now\n").unwrap();
        assert_eq!((d.kind, d.get("run")), (Kind::Run, "claude fix the build: it's red"));
        assert_eq!(
            d.lines()[2],
            "run: \"claude fix the build: it's red\"",
            "quoted, so it reads back"
        );
        let d = parse("kind: run\nin: /w\nrun: 'make test'\ntitle: \"the tests\"\n").unwrap();
        assert_eq!((d.get("run"), d.get("title")), ("make test", "the tests"));
        // The prompt itself carries `kind:` lines; an answer is the last block that does.
        let echoed =
            "kind: run          # something to do now, once\nin: <folder>\n\nkind: run\nin: /real\nrun: make it\n";
        assert_eq!(parse(echoed).unwrap().get("in"), "/real");
        assert!(
            parse("I cannot do that.")
                .unwrap_err()
                .to_string()
                .contains("no `kind:`")
        );
    }

    #[test]
    fn an_answer_of_another_kind_is_refused_by_name() {
        for kind in ["schedule", "flow", "poem"] {
            let err = parse(&format!("kind: {kind}\nrun: x")).unwrap_err().to_string();
            assert_eq!(err, format!("kind: `{kind}` is not run"));
        }
    }

    #[test]
    fn a_draft_fills_the_run_form() {
        let d = parse("kind: run\nin: /w\nrun: make test\ntitle: tests\n").unwrap();
        let f = form_of(&d, Path::new("/elsewhere"));
        assert_eq!(f.title(), "new run");
        assert_eq!(
            (f.get("in"), f.get("run"), f.get("title")),
            ("/w", "make test", "tests")
        );
        // Where it was typed, when the answer names no folder.
        let d = parse("kind: run\nrun: make test\n").unwrap();
        assert_eq!(form_of(&d, Path::new("/w/api")).get("in"), "/w/api");
    }
}

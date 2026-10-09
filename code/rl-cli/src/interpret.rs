//! Plain words → a draft: the context pack and the words go to
//! `assistants.interpret` as a run of that agent; what it answers — a run, or
//! a flow's `flow.md` — becomes the form, or a draft to read, for the
//! person's enter. Nothing here acts on its own: `apply` and `write_flow` are
//! called only after that enter, or `--go`.

use crate::client::Client;
use crate::context::{self, Here, Want};
use crate::flow::{Edited, NewFile};
use crate::tui::form::{self, Form};
use anyhow::{Context as _, Result, bail};
use rl_core::Home;
use rl_proto::flow::Reloaded;
use rl_proto::methods::{AssistantOutput, AssistantParams, AssistantRun, RunSpec, RunStart};
use rl_proto::types::{Cause, Run};
use std::collections::BTreeMap;
use std::path::Path;

/// What a run's draft is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
}

/// A flow's `flow.md` as drafted, and the name the assistant gave it, if it gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowDraft {
    pub name: Option<String>,
    pub text: String,
}

/// What the assistant answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Run(Draft),
    Flow(FlowDraft),
}

/// The answer in what the assistant said. A flow is the first `kind: flow` at the start of a
/// line — the pack's own shapes are indented, so an echo of it is never one — then its `name:`,
/// then its `flow.md` from a `---` line: to the end, or, when a fence opened before it, to the
/// last bare ```` ``` ```` line, the prose after dropped. Anything else is a run (`parse`).
pub fn answer(raw: &str) -> Result<Answer> {
    let lines: Vec<&str> = raw.lines().collect();
    let is_flow = |l: &&str| l.starts_with("kind:") && unquote(&strip_comment(l["kind:".len()..].trim())) == "flow";
    let Some(at) = lines.iter().position(is_flow) else {
        return parse(raw).map(Answer::Run);
    };
    let Some(dash) = lines[at..].iter().position(|l| l.trim_end() == "---").map(|i| at + i) else {
        bail!("kind: flow, then its flow.md from a `---` line: there is none");
    };
    let name = lines[at + 1..dash]
        .iter()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| unquote(&strip_comment(n.trim())))
        .filter(|n| !n.is_empty());
    // Inside a fence when an odd number of fence lines came before it.
    let fenced = lines[..at].iter().filter(|l| l.trim_start().starts_with("```")).count() % 2 == 1;
    let close = lines[dash..].iter().rposition(|l| l.trim() == "```").filter(|_| fenced);
    let end = close.map_or(lines.len(), |i| dash + i);
    let mut body: Vec<&str> = lines[dash..end].to_vec();
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    Ok(Answer::Flow(FlowDraft {
        name,
        text: format!("{}\n", body.join("\n")),
    }))
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
fn parse(raw: &str) -> Result<Draft> {
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
                "flow" => bail!("`kind: flow` starts its line, and its flow.md follows from a `---` line"),
                other => bail!("kind: `{other}` is not run or flow"),
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

/// Asks the interpreter: the pack for `folder` and what is wanted, then the words. What comes
/// back, read, with the run that said it. A whole flow takes longer to write than a run: three
/// minutes rather than the assistant's one.
pub async fn ask(home: &Home, folder: &Path, words: &str, want: &Want) -> Result<(Answer, AssistantOutput)> {
    let mut rpc = Client::connect(home).await?;
    let here: Here = context::gather(&mut rpc, folder).await?;
    let pack = context::render(context::help(), &here, want);
    let prompt = context::ask(&pack, words);
    let title = format!("interpret: {}", words.chars().take(48).collect::<String>());
    let timeout = matches!(want, Want::Flow { .. }).then(|| "3m".to_string());
    let out = rpc
        .call::<AssistantRun>(AssistantParams {
            job: "interpret".into(),
            prompt,
            cwd: folder.to_path_buf(),
            title,
            timeout,
        })
        .await?;
    if out.timed_out {
        bail!("{} took too long and was stopped (run {})", out.agent, out.run);
    }
    let said = answer(&out.output)
        .with_context(|| format!("{} answered (run {}):\n{}", out.agent, out.run, out.output.trim()))?;
    Ok((said, out))
}

/// A drafted flow written to its folder and, as it loads, said yes to: the person's enter, or
/// their `--go`. Only `flow.md`, in a new folder by a flow's name: a draft cannot reach past it,
/// nor over a flow there already.
pub async fn write_flow(home: &Home, name: &str, text: &str) -> Result<(Edited, Reloaded)> {
    let editing = crate::flow::start_new(home, name, vec![NewFile::flow_md(text)], true)?;
    let mut rpc = Client::connect(home).await?;
    crate::flow::finish_edit(&mut rpc, home, editing).await
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

    /// The run a raw answer is, or the test fails saying what it was instead.
    fn run(raw: &str) -> Draft {
        match answer(raw).unwrap() {
            Answer::Run(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_answer_is_found_among_fences_and_prose() {
        let raw = "Sure — here is a draft:\n```yaml\nkind: run\nin: ~/w/relay\nrun: make test\ntitle: tests\n```\nLet me know.";
        let d = run(raw);
        assert_eq!(d.kind, Kind::Run);
        assert_eq!(d.get("run"), "make test");
        assert_eq!(
            d.lines(),
            ["kind: run", "in: ~/w/relay", "run: make test", "title: tests"]
        );
        let d = run("kind: run\nin: /w\nrun: claude fix the build: it's red   # now\n");
        assert_eq!((d.kind, d.get("run")), (Kind::Run, "claude fix the build: it's red"));
        assert_eq!(
            d.lines()[2],
            "run: \"claude fix the build: it's red\"",
            "quoted, so it reads back"
        );
        let d = run("kind: run\nin: /w\nrun: 'make test'\ntitle: \"the tests\"\n");
        assert_eq!((d.get("run"), d.get("title")), ("make test", "the tests"));
        // The prompt itself carries `kind:` lines; an answer is the last block that does.
        let echoed =
            "kind: run          # something to do now, once\nin: <folder>\n\nkind: run\nin: /real\nrun: make it\n";
        assert_eq!(run(echoed).get("in"), "/real");
        assert!(
            answer("I cannot do that.")
                .unwrap_err()
                .to_string()
                .contains("no `kind:`")
        );
    }

    #[test]
    fn an_answer_of_another_kind_is_refused_by_name() {
        for kind in ["reminder", "poem"] {
            let err = answer(&format!("kind: {kind}\nrun: x")).unwrap_err().to_string();
            assert_eq!(err, format!("kind: `{kind}` is not run or flow"));
        }
    }

    #[test]
    fn a_flow_is_read_from_its_kind_line_to_the_end() {
        let raw = "Sure:\n```markdown\nkind: flow\nname: hello\n---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n```\n";
        assert_eq!(
            answer(raw).unwrap(),
            Answer::Flow(FlowDraft {
                name: Some("hello".into()),
                text: "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello.\n".into(),
            }),
            "the fence around it dropped, the text from its --- kept as written"
        );
        // The pack's shapes are indented: echoed above a run, they are not an answer.
        let echoed = "  kind: flow\n  name: <name>\n  ---\n\nkind: run\nin: /w\nrun: make it\n";
        assert!(matches!(answer(echoed).unwrap(), Answer::Run(d) if d.get("in") == "/w"));
        // A run's lines in a flow's prose are the flow's.
        let prose = "kind: flow\nname: x\n---\nsteps:\n  a:\n    run: echo\n---\nkind: run\n";
        assert!(matches!(answer(prose).unwrap(), Answer::Flow(_)));
        // No name: the person's to give.
        let Answer::Flow(d) = answer("kind: flow\n---\nsteps: {}\n---\n").unwrap() else {
            panic!("a flow")
        };
        assert_eq!(d.name, None);
        let e = answer("kind: flow\nname: x\nwhen: every 1h\n").unwrap_err().to_string();
        assert_eq!(e, "kind: flow, then its flow.md from a `---` line: there is none");
        // Quoted, as YAML allows.
        for k in ["\"flow\"", "'flow'"] {
            let raw = format!("kind: {k}\nname: x\n---\nsteps: {{}}\n---\n");
            assert!(matches!(answer(&raw).unwrap(), Answer::Flow(_)), "{k}");
        }
    }

    #[test]
    fn a_fenced_flow_ends_at_its_fence() {
        let md = "---\nsteps:\n  look:\n    agent: claude\n---\n\n## look\nRun this:\n```sh\nmake test\n```\n";
        let flow = |raw: &str| match answer(raw).unwrap() {
            Answer::Flow(d) => d.text,
            other => panic!("{other:?}"),
        };
        // Prose after the closing fence is not the flow's.
        let raw = format!("Here:\n```markdown\nkind: flow\nname: t\n{md}```\n\nLet me know if you want changes.\n");
        assert_eq!(
            flow(&raw),
            md,
            "the prompt's own code block kept, the prose after dropped"
        );
        // Not fenced: a prompt that ends in its own code block keeps its closing fence.
        assert_eq!(flow(&format!("kind: flow\nname: t\n{md}\n")), md);
    }

    #[test]
    fn a_draft_fills_the_run_form() {
        let d = run("kind: run\nin: /w\nrun: make test\ntitle: tests\n");
        let f = form_of(&d, Path::new("/elsewhere"));
        assert_eq!(f.title(), "new run");
        assert_eq!(
            (f.get("in"), f.get("run"), f.get("title")),
            ("/w", "make test", "tests")
        );
        // Where it was typed, when the answer names no folder.
        let d = run("kind: run\nrun: make test\n");
        assert_eq!(form_of(&d, Path::new("/w/api")).get("in"), "/w/api");
    }

    #[tokio::test]
    async fn a_draft_is_written_only_to_a_new_flow_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("h"));
        let text = "---\nsteps:\n  a:\n    run: echo\n---\n";
        // A name the assistant gave is a flow's name, or nothing is written: refused before
        // the daemon is reached, so none is needed here.
        for name in ["../../escaped", "/tmp/abs", "a/b", ".."] {
            let e = write_flow(&home, name, text).await.err().expect("refused").to_string();
            assert!(e.contains("is not a flow name"), "{name}: {e}");
        }
        assert!(!tmp.path().join("escaped").exists());
        assert!(!home.flow_dir("").exists(), "nothing written under flows/");
        // Nor over a flow there already.
        let mine = home.flow_dir("mine");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("flow.md"), "mine\n").unwrap();
        let e = write_flow(&home, "mine", text)
            .await
            .err()
            .expect("refused")
            .to_string();
        assert!(e.contains("is a flow already"), "{e}");
        assert_eq!(std::fs::read_to_string(mine.join("flow.md")).unwrap(), "mine\n");
    }
}

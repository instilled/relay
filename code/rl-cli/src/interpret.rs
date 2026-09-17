//! Plain words → a draft: the context pack and the words go to
//! `assistants.interpret` as a run of that agent; the one YAML document it
//! answers with becomes the form, filled in, for the person's enter. Nothing
//! here acts on its own: `apply` is called only after that enter.

use crate::client::Client;
use crate::context::{self, Here};
use crate::rulefile;
use crate::tui::form::{self, Form};
use anyhow::{Context as _, Result, bail};
use rl_core::Home;
use rl_proto::methods::{AssistantOutput, AssistantParams, AssistantRun, RunSpec, RunStart};
use rl_proto::rule::Rule;
use rl_proto::types::{Cause, Run};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
    Schedule,
    Reaction,
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
                Kind::Schedule => "schedule",
                Kind::Reaction => "reaction",
            }
        )];
        for key in ["in", "name", "when", "run", "title", "tags", "cwd"] {
            if let Some(val) = self.fields.get(key)
                && !val.is_empty()
            {
                v.push(format!("{key}: {}", rulefile_scalar(val)));
            }
        }
        v
    }
}

fn rulefile_scalar(s: &str) -> String {
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
                "schedule" => Kind::Schedule,
                "reaction" => Kind::Reaction,
                other => bail!("kind: `{other}` is not run, schedule or reaction"),
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

/// The form for a draft: a run form or a rule form, filled in. `editing`
/// keeps the rule the words were about, so saving rewrites its block.
pub fn form_of(d: &Draft, origin: &Path, editing: Option<&Rule>) -> Form {
    let place = d.get("in");
    let mut f = match (d.kind, editing) {
        (Kind::Run, _) => Form::run(origin),
        (_, Some(r)) => Form::edit(r),
        (_, None) => {
            let folder = (!form::is_global(place)).then(|| form::expand(place));
            Form::rule(folder.as_deref(), origin)
        }
    };
    if !place.is_empty() {
        f.set("in", place);
    }
    for key in ["name", "when", "run", "title", "tags", "cwd"] {
        let v = d.get(key);
        if !v.is_empty() {
            f.set(key, v);
        }
    }
    if d.kind != Kind::Run && d.get("when").is_empty() {
        // The kind says what `when` must start with; the assistant may have
        // left it for the person.
        f.set("when", if d.kind == Kind::Reaction { "on " } else { "every " });
    }
    f
}

/// Asks the interpreter: the pack for `folder` (and the rule, when editing),
/// then the words. What comes back, parsed, with the run that said it.
pub async fn ask(home: &Home, folder: &Path, words: &str, editing: Option<&Rule>) -> Result<(Draft, AssistantOutput)> {
    let mut rpc = Client::connect(home).await?;
    let here: Here = context::gather(&mut rpc, folder).await?;
    let pack = context::render(context::help(), &here);
    let editing_yaml = editing.map(|r| {
        let body = match r {
            Rule::Schedule(s) => rulefile::body(&s.spec, &Default::default()),
            Rule::Reaction(x) => rulefile::body(&x.spec, &Default::default()),
        }
        .unwrap_or_default();
        format!(
            "{}:\n{}",
            r.name(),
            body.iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
        )
    });
    let prompt = context::ask(
        &pack,
        words,
        editing.map(|r| (r.kind().to_string(), r.scope_label())).as_ref(),
        editing_yaml.as_deref(),
    );
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

/// What the form says, done: a run started, or a rule installed. The
/// person's yes, on the line. Returns a line saying what happened.
pub async fn apply(home: &Home, f: &Form, registered: impl Fn(&Path) -> bool) -> Result<String> {
    let mut rpc = Client::connect(home).await?;
    match f.kind {
        form::Kind::Run => {
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
                flow: None,
            };
            let run: Run = rpc.call::<RunStart>(spec).await?;
            Ok(format!("started {} · {}", run.title, run.id))
        }
        form::Kind::Rule => {
            let d = form::rule_draft(f)?;
            let known = d.folder.as_deref().is_none_or(&registered);
            let was = f.editing.as_ref().map(form::Editing::was);
            let file = rulefile::install(home, &mut rpc, &d, was.as_ref(), known).await?;
            Ok(format!("{} on · {}", d.name, rl_core::display_path(&file)))
        }
        form::Kind::Rename => {
            let name = f.get("name").trim().to_string();
            let w = rpc
                .call::<rl_proto::methods::WorkspaceRename>(rl_proto::methods::WorkspaceRenameParams {
                    path: f.origin.clone(),
                    name,
                })
                .await?;
            Ok(format!("renamed to {}", w.name))
        }
        // Words never make one: a run is picked from the list to be renamed,
        // and `form_of` only ever builds a run or a rule.
        form::Kind::RenameRun => anyhow::bail!("a run is renamed from the panel"),
    }
}

/// Where `in` points, for `apply`'s `registered`.
pub fn folder_of(f: &Form) -> Option<PathBuf> {
    let p = f.get("in");
    (!form::is_global(p)).then(|| form::expand(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_found_among_fences_and_prose() {
        let raw = "Sure — here is a draft:\n```yaml\nkind: schedule\nin: ~/w/relay\nname: nightly\nwhen: every 4h\nrun: make test\ntags: [ci, slow]\n```\nLet me know.";
        let d = parse(raw).unwrap();
        assert_eq!(d.kind, Kind::Schedule);
        assert_eq!(d.get("name"), "nightly");
        assert_eq!(d.get("tags"), "ci, slow");
        assert_eq!(
            d.lines(),
            [
                "kind: schedule",
                "in: ~/w/relay",
                "name: nightly",
                "when: every 4h",
                "run: make test",
                "tags: ci, slow"
            ]
        );
        let d = parse("kind: run\nin: /w\nrun: claude fix the build: it's red   # now\n").unwrap();
        assert_eq!((d.kind, d.get("run")), (Kind::Run, "claude fix the build: it's red"));
        let d =
            parse("kind: schedule\nname: n\nwhen: \"0 2 * * *\"\nrun: 'make test'\ntags:\n  - ci\n  - slow\n").unwrap();
        assert_eq!(
            (d.get("when"), d.get("run"), d.get("tags")),
            ("0 2 * * *", "make test", "ci, slow")
        );
        // The prompt itself carries `kind:` lines; an answer is the last block that does.
        let echoed = "kind: run          # something to do now, once\nin: <folder>\n\nkind: schedule\nin: <folder, or global>\n\nkind: run\nin: /real\nrun: make it\n";
        assert_eq!(parse(echoed).unwrap().get("in"), "/real");
        assert!(
            parse("I cannot do that.")
                .unwrap_err()
                .to_string()
                .contains("no `kind:`")
        );
        assert!(
            parse("kind: poem\nrun: x")
                .unwrap_err()
                .to_string()
                .contains("not run, schedule")
        );
    }

    #[test]
    fn a_draft_fills_the_right_form() {
        let d = parse("kind: run\nin: /w\nrun: make test\ntitle: tests\n").unwrap();
        let f = form_of(&d, Path::new("/w"), None);
        assert_eq!(f.kind, form::Kind::Run);
        assert_eq!(
            (f.get("in"), f.get("run"), f.get("title")),
            ("/w", "make test", "tests")
        );

        let d = parse("kind: reaction\nin: global\nname: notify\nwhen: on run.failed\nrun: say no\ncwd: /w\n").unwrap();
        let f = form_of(&d, Path::new("/w"), None);
        assert_eq!(f.kind, form::Kind::Rule);
        assert_eq!(
            (f.get("in"), f.get("when"), f.get("cwd")),
            ("global", "on run.failed", "/w")
        );

        // A rule kind with `when` left out gets its prefix, so the person sees what is missing.
        let d = parse("kind: schedule\nname: x\nrun: y\n").unwrap();
        assert_eq!(form_of(&d, Path::new("/w"), None).get("when"), "every");
    }
}

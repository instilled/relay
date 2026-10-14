use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use jiff::SignedDuration;
use rl_cli::client::Client;
use rl_cli::context::Want;
use rl_cli::daemon;
use rl_cli::env::child_env;
use rl_cli::flow::FlowCmd;
use rl_cli::interpret::Answer;
use rl_cli::render;
use rl_core::time::{now, parse_since};
use rl_core::{Home, display_path};
use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
use rl_proto::flow::{FlowFilter, FlowList};
use rl_proto::methods::{
    AgentList, ConfigGet, GcParams, GcRun, RunArchive, RunArchiveParams, RunHide, RunHideParams, RunResume,
    RunResumeParams, SummaryGet, SummaryMake, SummaryParams,
};
use rl_proto::methods::{
    DaemonInfo, DigestGet, DigestHide, DigestHideParams, DigestParams, EventFilter, EventList, PresenceClient,
    PresenceTouch, RunFilter, RunList, RunMark, RunMarkParams, RunSpec, RunStart, RunStop, RunStopParams,
};
use rl_proto::methods::{
    DecisionDismiss, DecisionFilter, DecisionGet, DecisionId, DecisionList, DecisionNote, DecisionRaise, DecisionSpec,
    DecisionTake, DecisionTakeParams,
};
use rl_proto::methods::{
    EventEmit, EventEmitParams, WorkspaceList, WorkspaceRemove, WorkspaceRemoveParams, WorkspaceRemoved, WorkspaceUp,
    WorkspaceUpParams,
};
use rl_proto::types::{Cause, Mark, RunKind, RunState, SummaryTarget};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long `rl daemon stop --with-runs` waits for a holder to hang up after
/// being asked to go, before moving on to the next run.
const GOING: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(
    name = "rl",
    version,
    about = "Relay — keeps your agents and long-running work alive",
    after_help = "Every command starts rld if it is not running, and says so when the one running is another release.\nA run and its events are never deleted: `rl archive` puts a run away, `rl ls --all` still has it. What does go is the journal — the terminal bytes of a run that ended over `retention` ago, 30d unless config.yaml says otherwise (`never` keeps them; `rl prune` runs it now).\n\nFor agents:\n  rl context    what Relay is, its events and flows, and the answer it wants — read this first\n  rl agents     the agent CLIs on this machine\n  rl --json …   every command, machine-readable"
)]
struct Cli {
    /// Machine-readable output on every command.
    #[arg(long, global = true)]
    json: bool,
    /// With no command: the control panel — everything ranked by attention
    /// on the left, the selected thing on the right, its terminal on enter.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the rld daemon.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Start a run: a command, or an agent CLI with --agent. Returns immediately with the run id.
    Run(RunArgs),
    /// Stop a run (SIGTERM, a shell SIGHUP; --force for SIGKILL).
    Stop(StopArgs),
    /// List runs (active by default).
    Ls(LsArgs),
    /// Archive a run: out of the panel and `rl since`, kept in the ledger. Ended runs only.
    #[command(alias = "done")]
    Archive(ArchiveArgs),
    /// Take runs, or a project and everything in it, off the panel's list: shown only in its `hidden` lens, unless they come to need you. Nothing is stopped.
    Hide(HideArgs),
    /// Bring back what `rl hide` took off the panel's list.
    Unhide(HideArgs),
    /// Pick an ended run back up as a new run: an agent's conversation, a fresh shell, a command again.
    Resume(ResumeArgs),
    /// What happened while you were away: the panel's catch-up — a summary on top, then each project and what in it needs you, ran or failed.
    Since(SinceArgs),
    /// List ledger events.
    Events(EventsArgs),
    /// Flows: what starts work without you, in ~/.relay/flows. List, edit, run, pause, approve.
    Flow {
        #[command(subcommand)]
        cmd: FlowCmd,
    },
    /// Your defaults (~/.relay/config.yaml) as they stand, each assistant resolved; `edit` opens the file.
    Config(ConfigArgs),
    /// The agent CLIs Relay knows, and which are on this machine.
    Agents,
    /// The setup guide: which agents the panel offers, who writes summaries.
    Setup,
    /// Move Claude's own sessions (`claude --bg`) into Relay.
    Import,
    /// What Relay tells an agent: the words, events, the flows and folders here, and the answer it wants.
    Context(ContextArgs),
    /// Plain words to `assistants.interpret`: a run or a flow, drafted; `--go` starts the run, or writes the flow and says yes.
    Interpret(InterpretArgs),
    /// A recap of a run, live or ended: its agent's own (Claude), its turns (Codex), or `assistants.summary`'s. Kept by the run.
    Summary(SummaryArgs),
    /// Let the journal of old runs go, and their firings' folders, as `retention:` says (30d unless set, or --older-than). Runs and events stay.
    #[command(alias = "gc")]
    Prune(PruneArgs),
    /// Emit a custom event, from a command or plugin; flows that fire `on` it pick it up.
    Emit(EmitArgs),
    /// Ask a human something and carry on: the question waits until answered.
    Ask(AskArgs),
    /// Questions waiting on you (and, with --all, the answered ones).
    Decisions(DecisionsArgs),
    /// Answer one: choose an option, or dismiss it.
    Decide(DecideArgs),
    /// Register a folder as a project Relay knows, so flows can run in it.
    #[command(visible_alias = "add")]
    Up(UpArgs),
    /// Registered folders (see `rl up`).
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Be in a run's terminal: keyboard, resize, the lot. ctrl-\ to detach; the run keeps going.
    Join(AttachArgs),
    /// Say what a run is doing when its CLI cannot: done, blocked or working.
    Mark(MarkArgs),
    /// Print the prompt-mark snippet for your shell, so a project shell can tell one command from the next.
    ShellInit(ShellInitArgs),
    /// Called by an agent CLI's hooks (`rl hook <agent> <Event>`) with the hook's JSON on stdin. Never fails the CLI.
    #[command(hide = true, disable_help_flag = true)]
    Hook(HookArgs),
}

#[derive(Args)]
struct ShellInitArgs {
    /// zsh or bash. Defaults to whatever `$SHELL` says you use.
    shell: Option<String>,
}

#[derive(Args)]
struct AttachArgs {
    /// Run id.
    id: String,
    /// Join even if someone else has; they become a watcher.
    #[arg(long)]
    take: bool,
    /// KiB of journaled output to draw from (an ended or pipes run).
    #[arg(long, default_value_t = 256, value_name = "KIB")]
    scrollback: u64,
}

#[derive(Args)]
struct MarkArgs {
    /// Run id.
    id: String,
    /// done (nothing more for you here → idle), blocked, or working.
    mark: Mark,
    /// Why, shown as the event summary.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Args)]
struct HookArgs {
    /// `<agent> <Event>` — the agent whose hook it is (claude, codex,
    /// gemini), then the hook event name, e.g. Notification or Stop — or an
    /// event alone, which is Claude's; `--user` after them for the copy in
    /// the user's own settings. Anything else is taken, and ignored: see
    /// `rl_cli::hook::words`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    words: Vec<String>,
}

#[derive(Args)]
struct PruneArgs {
    /// Runs ended longer ago than this (default: the config's retention, or 30d).
    #[arg(long, value_name = "DURATION")]
    older_than: Option<String>,
    /// Only say what would go.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct InterpretArgs {
    /// What you want, in plain words.
    #[arg(required = true)]
    words: Vec<String>,
    /// The folder the words are about (default: here).
    #[arg(long = "in", value_name = "FOLDER")]
    folder: Option<PathBuf>,
    /// Do what the draft says, without showing the form.
    #[arg(long)]
    go: bool,
}

#[derive(Args)]
struct SummaryArgs {
    /// What: a run, a folder (its project), `flow:<name>`, `firing:<id>`, or `all`.
    what: String,
    /// Only the stretch since: a duration back (30m, 2h), a date or an
    /// instant (default: a run, since you last looked, if it did anything
    /// since, else all of it; a flow, since its oldest run you have not
    /// seen; a project or all, the last day).
    since: Option<String>,
    /// Read the logs, not the recaps: slower, and says more.
    #[arg(long)]
    deep: bool,
    /// Make a new one, even when the one kept is up to date.
    #[arg(long)]
    again: bool,
}

#[derive(Args)]
struct ContextArgs {
    /// The folder the words would be typed in (default: here).
    #[arg(long = "in", value_name = "FOLDER")]
    folder: Option<PathBuf>,
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    cmd: Option<ConfigCmd>,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Open config.yaml in $VISUAL / $EDITOR, then show it as the daemon reads it.
    Edit,
}

#[derive(Args)]
struct EmitArgs {
    /// The event kind — your own dotted name, like `log.issue`. The daemon's
    /// own families (`run.*`, `decision.*`, …) are refused.
    kind: String,
    /// A field of the event's data: `-d severity=ERROR`. Repeatable.
    #[arg(short = 'd', long = "data", value_name = "KEY=VALUE")]
    data: Vec<String>,
    /// Read the whole data object as JSON — a string, or `-` for stdin.
    #[arg(long = "json-data", value_name = "JSON")]
    json_data: Option<String>,
    /// A one-line summary (default: the kind).
    #[arg(short, long)]
    summary: Option<String>,
    /// The folder it is about (default: $RELAY_RUN's folder, else here).
    #[arg(long = "in", value_name = "DIR")]
    in_: Option<PathBuf>,
    /// The run it belongs to (default: $RELAY_RUN, when there is one).
    #[arg(long, value_name = "ID")]
    run: Option<String>,
    /// Override the group key it is filed under.
    #[arg(long)]
    group: Option<String>,
}

#[derive(Args)]
struct AskArgs {
    /// The question, in one line. Left out, the whole thing is read as YAML
    /// (or JSON) on stdin: title, summary, options with actions.
    title: Option<String>,
    /// An option: `key`, or `key:Label`. Repeatable. An option written this
    /// way is an answer, not a next step; give it an action through stdin.
    #[arg(short = 'o', long = "option", value_name = "KEY[:LABEL]")]
    options: Vec<String>,
    /// The longer story, for whoever reads the question.
    #[arg(short, long)]
    summary: Option<String>,
    /// Several options may be chosen at once.
    #[arg(long)]
    multi: bool,
    /// Asking the same thing again replaces the open question with this key.
    #[arg(short, long)]
    group: Option<String>,
    /// How long it stands (`4h`, `2d`) before the default is taken or it expires.
    #[arg(short = 'e', long, value_name = "DURATION")]
    expires: Option<String>,
    /// Which option is taken if it expires.
    #[arg(long = "default", value_name = "KEY")]
    default_option: Option<String>,
    /// The run asking (default: $RELAY_RUN, when there is one).
    #[arg(long, value_name = "ID")]
    run: Option<String>,
    /// Where its options run, when no run says (default: here).
    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Args)]
struct DecisionsArgs {
    /// Include the ones already answered.
    #[arg(short, long)]
    all: bool,
    /// Only the ones a run asked.
    #[arg(long, value_name = "ID")]
    run: Option<String>,
    /// At most N, newest first.
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: u32,
}

#[derive(Args)]
struct DecideArgs {
    /// Decision id.
    id: String,
    /// Option keys. More than one only when the question said `multi`.
    options: Vec<String>,
    /// Answer "not this": nothing runs.
    #[arg(short, long)]
    dismiss: bool,
    /// Why, kept with the answer.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Args)]
struct UpArgs {
    /// The folder (default: current).
    dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// List registered folders.
    Ls,
    /// Unregister a folder: flows in it no longer load, its open questions are dismissed.
    Rm(WorkspaceRmArgs),
}

#[derive(Args)]
struct WorkspaceRmArgs {
    dir: PathBuf,
    /// Also stop every run still going in it — shells and agents alike.
    #[arg(long)]
    stop: bool,
    /// Also archive its ended runs (those just stopped included).
    #[arg(long)]
    archive: bool,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start rld if it is not already running.
    Start,
    /// Ask rld to exit. Running sessions are untouched unless --with-runs.
    Stop {
        /// Stop the runs first, so nothing is left holding a terminal.
        #[arg(long)]
        with_runs: bool,
    },
    /// Show whether rld is running.
    Status,
    /// Stop then start. Running sessions are untouched.
    Restart,
}

#[derive(Args)]
#[command(override_usage = "rl run [OPTIONS] -- <COMMAND>...\n       rl run --agent <CLI> [OPTIONS] [PROMPT]...")]
struct RunArgs {
    /// Run an agent CLI (claude, codex, …) instead of a plain command; the rest is its prompt/args.
    #[arg(long, value_name = "CLI")]
    agent: Option<String>,
    /// Title shown in lists (default: the command line).
    #[arg(long)]
    title: Option<String>,
    /// What this run is part of, e.g. notion:FL-123 or gh:42 (default: $RELAY_REF).
    #[arg(long, value_name = "ID")]
    r#ref: Option<String>,
    /// Label for filtering; key:value allowed (repeatable; default: $RELAY_TAGS).
    #[arg(long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Working directory (default: current).
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Pipes instead of a pty, for headless runs.
    #[arg(long)]
    no_tty: bool,
    /// Extra environment for the run, KEY=VALUE (repeatable).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// The command (or, with --agent, the prompt and arguments).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct StopArgs {
    /// Run id.
    id: String,
    /// SIGKILL instead of SIGTERM.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct LsArgs {
    /// Include finished, failed and stopped runs, and those you are done with.
    #[arg(short, long)]
    all: bool,
    /// Only runs in this state (repeatable).
    #[arg(long = "state", value_name = "STATE")]
    states: Vec<RunState>,
    /// Only runs in this group (ref, branch or workspace).
    #[arg(long)]
    group: Option<String>,
    /// Only runs carrying this tag.
    #[arg(long)]
    tag: Option<String>,
    /// Only runs with this cause, e.g. flow:nightly/probe.
    #[arg(long, value_name = "CAUSE")]
    cause: Option<String>,
    /// Newest N.
    #[arg(short = 'n', long, default_value_t = 200)]
    limit: u32,
}

#[derive(Args)]
struct ArchiveArgs {
    /// Run ids.
    ids: Vec<String>,
    /// Instead: everything that has ended and been looked at (the panel's `/archive seen`).
    #[arg(long, alias = "seen", conflicts_with = "ids")]
    all: bool,
}

#[derive(Args)]
struct HideArgs {
    /// Run ids.
    ids: Vec<String>,
    /// A project instead, by its folder: everything in it, runs to come too.
    #[arg(long, value_name = "DIR", conflicts_with = "ids")]
    project: Option<PathBuf>,
}

#[derive(Args)]
struct ResumeArgs {
    /// Run ids.
    ids: Vec<String>,
    /// Instead: every run lost in a restart that is still on the list — agents and shells.
    #[arg(long, conflicts_with = "ids")]
    lost: bool,
    /// With --lost: plain commands too, run again.
    #[arg(long, requires = "lost")]
    commands: bool,
}

#[derive(Args)]
struct SinceArgs {
    /// The boundary: a duration (8h), a date, or an instant. Default: when you last looked.
    since: Option<String>,
}

#[derive(Args)]
struct EventsArgs {
    /// Only events after this point: a duration (8h), a date (2026-09-08) or an RFC 3339 instant.
    #[arg(long)]
    since: Option<String>,
    /// Only events of this run.
    #[arg(long)]
    run: Option<String>,
    /// Only events in this group.
    #[arg(long)]
    group: Option<String>,
    /// Only this kind (repeatable), e.g. run.failed.
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,
    /// At most N, oldest first.
    #[arg(short = 'n', long, default_value_t = 500)]
    limit: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(words) = rl_cli::hook::args() {
        return match Home::resolve() {
            Ok(home) => hook(&home, &words).await,
            Err(_) => Ok(()),
        };
    }
    let cli = Cli::parse();
    let home = Home::resolve()?;
    rl_cli::context::set_help(help_for_the_pack());
    let Some(cmd) = cli.cmd else {
        if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            bail!("rl with no command is the control panel and needs a terminal; try `rl ls` or `rl --help`");
        }
        return rl_cli::tui::run(&home).await;
    };
    match cmd {
        Cmd::Daemon { cmd } => run_daemon(&home, cmd, cli.json).await,
        Cmd::Run(a) => run(&home, a, cli.json).await,
        Cmd::Stop(a) => stop(&home, a, cli.json).await,
        Cmd::Ls(a) => ls(&home, a, cli.json).await,
        Cmd::Archive(a) => archive(&home, a, cli.json).await,
        Cmd::Hide(a) => hide(&home, a, true, cli.json).await,
        Cmd::Unhide(a) => hide(&home, a, false, cli.json).await,
        Cmd::Resume(a) => resume(&home, a, cli.json).await,
        Cmd::Since(a) => since(&home, a, cli.json).await,
        Cmd::Events(a) => events(&home, a, cli.json).await,
        Cmd::Flow { cmd } => rl_cli::flow::flow(&home, cmd, cli.json).await,
        Cmd::Config(a) => config(&home, a, cli.json).await,
        Cmd::Agents => agents(&home, cli.json).await,
        Cmd::Import => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                bail!("rl import asks on the screen and needs a terminal; it moves sessions you pick");
            }
            rl_cli::tui::setup(&home, true).await
        }
        Cmd::Setup => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                bail!("rl setup asks on the screen and needs a terminal; `rl config edit` changes the file");
            }
            rl_cli::tui::setup(&home, false).await
        }
        Cmd::Context(a) => context(&home, a).await,
        Cmd::Interpret(a) => interpret(&home, a, cli.json).await,
        Cmd::Summary(a) => summary(&home, a, cli.json).await,
        Cmd::Prune(a) => prune(&home, a, cli.json).await,
        Cmd::Emit(a) => emit(&home, a, cli.json).await,
        Cmd::Ask(a) => ask(&home, a, cli.json).await,
        Cmd::Decisions(a) => decisions(&home, a, cli.json).await,
        Cmd::Decide(a) => decide(&home, a, cli.json).await,
        Cmd::Up(a) => up(&home, a, cli.json).await,
        Cmd::Workspace { cmd } => workspace(&home, cmd, cli.json).await,
        Cmd::Join(a) => attach(&home, a).await,
        Cmd::Mark(a) => mark(&home, a, cli.json).await,
        Cmd::ShellInit(a) => shell_init(a),
        Cmd::Hook(a) => hook(&home, &a.words).await,
    }
}

async fn attach(home: &Home, a: AttachArgs) -> Result<()> {
    let ended = rl_cli::attach::attach(
        home,
        rl_cli::attach::Opts {
            id: a.id,
            take: a.take,
            scrollback: a.scrollback * 1024,
        },
    )
    .await?;
    std::process::exit(ended.code())
}

async fn mark(home: &Home, a: MarkArgs, json: bool) -> Result<()> {
    let run = daemon::ensure(home, !json)
        .await?
        .call::<RunMark>(RunMarkParams {
            id: a.id,
            mark: a.mark,
            note: a.note,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&run)?);
    } else {
        println!("{} · {}", run.id, render::state_label(run.state, run.confidence));
    }
    Ok(())
}

/// A hook must never slow down or break the CLI that called it: words it
/// does not know do nothing, and `rl_cli::hook` swallows any trouble, prints
/// nothing and gives up within its time.
async fn hook(home: &Home, words: &[String]) -> Result<()> {
    if let Some(words) = rl_cli::hook::words(words) {
        rl_cli::hook::hook(home, &words).await;
    }
    Ok(())
}

async fn run(home: &Home, a: RunArgs, json: bool) -> Result<()> {
    let (kind, argv) = match a.agent {
        Some(cli) => {
            let mut argv = vec![cli];
            argv.extend(a.args);
            (RunKind::Agent, argv)
        }
        None => {
            if a.args.is_empty() {
                bail!("nothing to run: `rl run -- <command>` or `rl run --agent <cli> [prompt]`");
            }
            (RunKind::Command, a.args)
        }
    };
    let env = child_env(&a.env)?;
    // Sub-runs started by agents or plugins inherit their group from the environment.
    let ref_ = a
        .r#ref
        .or_else(|| std::env::var("RELAY_REF").ok().filter(|s| !s.is_empty()));
    let tags = if a.tags.is_empty() {
        std::env::var("RELAY_TAGS")
            .ok()
            .map(|s| s.split(',').filter(|t| !t.is_empty()).map(String::from).collect())
            .unwrap_or_default()
    } else {
        a.tags
    };
    let cwd = match a.cwd {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let (cols, rows) = terminal_size();
    let spec = RunSpec {
        argv,
        kind,
        cwd,
        title: a.title,
        ref_,
        tags,
        tty: !a.no_tty,
        cols,
        rows,
        env,
        inherit_env: false,
        input: true,
        cause: Cause::Manual,
        chain: None,
    };
    let run = daemon::ensure(home, !json).await?.call::<RunStart>(spec).await?;
    if json {
        println!("{}", serde_json::to_string(&run)?);
    } else {
        println!("{}", run.id);
    }
    Ok(())
}

async fn up(home: &Home, a: UpArgs, json: bool) -> Result<()> {
    let path = match a.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let mut rpc = daemon::ensure(home, !json).await?;
    let w = rpc.call::<WorkspaceUp>(WorkspaceUpParams { path }).await?;
    if json {
        println!("{}", serde_json::to_string(&w)?);
    } else {
        println!("workspace {} ({})", w.name, display_path(&w.path));
    }
    Ok(())
}

async fn workspace(home: &Home, cmd: WorkspaceCmd, json: bool) -> Result<()> {
    let mut rpc = daemon::ensure(home, !json).await?;
    match cmd {
        WorkspaceCmd::Ls => {
            let list = rpc.call::<WorkspaceList>(()).await?;
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!("no workspaces; `rl up` in a folder registers it");
            } else {
                let now = now();
                let rows: Vec<Vec<String>> = list
                    .iter()
                    .map(|w| {
                        vec![
                            w.name.clone(),
                            display_path(&w.path),
                            format!("{} ago", rl_core::time::fmt_age(w.registered_at, now)),
                        ]
                    })
                    .collect();
                println!("{}", render::table(&["NAME", "PATH", "REGISTERED"], &rows));
            }
        }
        WorkspaceCmd::Rm(a) => {
            // The daemon's cwd is not ours: a relative folder is made whole here.
            let dir = std::path::absolute(&a.dir).unwrap_or_else(|_| a.dir.clone());
            let r = rpc
                .call::<WorkspaceRemove>(WorkspaceRemoveParams {
                    path: dir.clone(),
                    stop_runs: a.stop,
                    archive_runs: a.archive,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("{}", removed_words(&r));
            }
        }
    }
    Ok(())
}

/// What `rl workspace rm` did, in a line: whether the folder was registered,
/// then only what was done besides.
fn removed_words(r: &WorkspaceRemoved) -> String {
    let mut words = vec![if r.removed {
        format!("{} unregistered", display_path(&r.path))
    } else {
        format!("{} was not registered", display_path(&r.path))
    }];
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    if r.stopped > 0 {
        words.push(format!("stopped {}", n(r.stopped, "run", "runs")));
    }
    if r.still_running > 0 {
        words.push(format!("{} still going", n(r.still_running, "run", "runs")));
    }
    if r.archived > 0 {
        words.push(format!("{} archived", n(r.archived, "run", "runs")));
    }
    if r.dismissed > 0 {
        words.push(format!("{} dismissed", n(r.dismissed, "question", "questions")));
    }
    words.join(" · ")
}

async fn stop(home: &Home, a: StopArgs, json: bool) -> Result<()> {
    let run = daemon::ensure(home, !json)
        .await?
        .call::<RunStop>(RunStopParams {
            id: a.id,
            force: a.force,
            archive: false,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&run)?);
    } else {
        println!(
            "{} · stop requested ({})",
            run.id,
            if a.force {
                "SIGKILL"
            } else if run.is_shell() {
                "SIGHUP"
            } else {
                "SIGTERM"
            }
        );
    }
    Ok(())
}

/// The caller's terminal size, so a pty run starts at a sensible width.
fn terminal_size() -> (Option<u16>, Option<u16>) {
    let mut ws: libc_winsize = libc_winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // TIOCGWINSZ on stdout; fall back to nothing when not a tty.
    let ok = unsafe { ioctl(1, TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (Some(ws.ws_col), Some(ws.ws_row))
    } else {
        (None, None)
    }
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct libc_winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}
#[cfg(target_os = "macos")]
const TIOCGWINSZ: u64 = 0x40087468;
#[cfg(target_os = "linux")]
const TIOCGWINSZ: u64 = 0x5413;
unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

async fn ls(home: &Home, a: LsArgs, json: bool) -> Result<()> {
    let filter = RunFilter {
        states: a.states,
        active_only: !a.all,
        archived: a.all,
        workspace: None,
        group: a.group,
        tag: a.tag,
        cause: a.cause,
        limit: Some(a.limit),
    };
    let runs = daemon::ensure(home, !json).await?.call::<RunList>(filter).await?;
    if json {
        println!("{}", serde_json::to_string(&runs)?);
    } else if runs.is_empty() {
        println!("no runs");
    } else {
        print!(
            "{}",
            render::table(&render::RUN_HEADERS, &render::run_rows(&runs, now()))
        );
    }
    Ok(())
}

/// `rl archive` (once `rl done`): the panel's `/archive`, from the line.
async fn archive(home: &Home, a: ArchiveArgs, json: bool) -> Result<()> {
    if a.ids.is_empty() && !a.all {
        bail!("say which: `rl archive <id>…`, or `rl archive --all` for everything ended and seen");
    }
    let n = daemon::ensure(home, !json)
        .await?
        .call::<RunArchive>(RunArchiveParams {
            ids: a.ids.clone(),
            all_seen: a.all,
        })
        .await?;
    if json {
        println!("{}", json!({ "archived": n }));
    } else if n == 0 && !a.all {
        bail!("nothing archived: not found, still running, or already archived");
    } else {
        println!("{n} archived");
    }
    Ok(())
}

/// `rl hide` and `rl unhide`: the panel's `/hide`, over `run.hide` for runs
/// and `digest.hide` for a project, which is hidden by its header's key.
async fn hide(home: &Home, a: HideArgs, hidden: bool, json: bool) -> Result<()> {
    let verb = if hidden { "hide" } else { "unhide" };
    let mut rpc = daemon::ensure(home, !json).await?;
    if let Some(dir) = a.project {
        let dir = std::fs::canonicalize(&dir).with_context(|| format!("{}: not a folder", dir.display()))?;
        let key = format!("g:{}", dir.display());
        let now = rpc.call::<DigestHide>(DigestHideParams { key, hidden }).await?;
        if json {
            println!("{}", serde_json::to_string(&now)?);
        } else {
            println!("{} {}", if hidden { "hid" } else { "brought back" }, dir.display());
        }
        return Ok(());
    }
    if a.ids.is_empty() {
        bail!("say which: `rl {verb} <id>…`, or `rl {verb} --project <dir>`");
    }
    let mut out = Vec::new();
    for id in a.ids {
        out.push(rpc.call::<RunHide>(RunHideParams { id, hidden }).await?);
    }
    if json {
        println!("{}", serde_json::to_string(&out)?);
    } else {
        for r in out {
            println!("{} · {}", r.id, if hidden { "hidden" } else { "on the list" });
        }
    }
    Ok(())
}

async fn resume(home: &Home, a: ResumeArgs, json: bool) -> Result<()> {
    if a.ids.is_empty() && !a.lost {
        bail!("say which: `rl resume <id>…`, or `rl resume --lost` for what a restart took");
    }
    let (cols, rows) = crossterm::terminal::size().map_or((None, None), |(c, r)| (Some(c), Some(r)));
    let out = daemon::ensure(home, !json)
        .await?
        .call::<RunResume>(RunResumeParams {
            ids: a.ids.clone(),
            lost: a.lost,
            commands: a.commands,
            env: child_env(&[])?,
            cols,
            rows,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }
    if out.is_empty() {
        println!("nothing to resume");
    }
    for r in &out {
        match (&r.run, &r.why_not) {
            (Some(new), _) => println!("{}  resumed {} as {}", r.from, r.title, new.id),
            (None, why) => println!("{}  {}: {}", r.from, r.title, why.as_deref().unwrap_or("not resumed")),
        }
    }
    if !a.ids.is_empty() && out.iter().all(|r| r.run.is_none()) {
        bail!("nothing resumed");
    }
    Ok(())
}

async fn since(home: &Home, a: SinceArgs, json: bool) -> Result<()> {
    let n = now();
    let mut c = daemon::ensure(home, !json).await?;
    // Bare, it counts from when you last looked — and only then is the top
    // written by itself, as you come back.
    let (boundary, came_back) = match a.since.as_deref() {
        Some(s) => (parse_since(s, n)?, false),
        None => {
            // Looking now; the boundary is the last time we did.
            let me = rl_cli::attach::client_name();
            match c
                .call::<PresenceTouch>(PresenceClient { client: me })
                .await?
                .last_seen_at
            {
                Some(last) => (last, true),
                None => (n - jiff::SignedDuration::from_hours(24), false),
            }
        }
    };
    // The stretch cut from the list's digest, as the panel cuts it, so the
    // two never disagree.
    let d = rl_cli::catchup::stretch(&c.call::<DigestGet>(DigestParams::default()).await?, boundary);
    if json {
        println!("{}", serde_json::to_string(&d)?);
        return Ok(());
    }
    // The catch-up as the panel builds it: the list's rows, grouped as the
    // list groups them.
    let workspaces = c.call::<WorkspaceList>(()).await?;
    let flows = c.call::<FlowList>(FlowFilter::default()).await?;
    let relay = std::fs::canonicalize(home.path()).unwrap_or_else(|_| home.path().to_path_buf());
    let cu = rl_cli::catchup::CatchUp::of(
        &d,
        workspaces.into_iter().map(|w| (w.path, w.name)),
        &flows,
        Some(&relay),
        n,
    );
    // What heads it: each `catch_up: top` flow's latest firing since, all it came to fetched.
    let leads = rl_cli::catchup::heads::leads(&flows, &d.firings, boundary);
    let mut outcomes = rl_cli::catchup::heads::Outcomes::default();
    outcomes
        .fetch(&mut c, leads.iter().map(|l| l.firing.clone()).collect())
        .await;
    let mut top = rl_cli::catchup::heads::text(&leads, &outcomes, n);
    top.extend(since_top(&mut c, &cu, boundary, came_back).await);
    print!("{}", rl_cli::catchup::text(&cu, &top, n));
    Ok(())
}

/// The top `rl since` prints, as `plan_top` says: the one shown, or one
/// asked for and waited on, or — with neither, or the assistant failing —
/// the counts.
async fn since_top(
    c: &mut Client,
    cu: &rl_cli::catchup::CatchUp<'_>,
    from: jiff::Timestamp,
    came_back: bool,
) -> Vec<String> {
    use rl_cli::catchup::{plan_top, top_worthy, written};
    let plan = plan_top(c, from, cu.has_runs(), came_back, now()).await;
    let top = match plan.ask {
        Some(ask) => {
            eprintln!("writing the top…");
            match c.call::<SummaryMake>(ask).await {
                Ok(s) => Some(s).filter(top_worthy),
                Err(e) => {
                    eprintln!("the top: {e:#}");
                    None
                }
            }
        }
        None => plan.shown,
    };
    match top {
        Some(s) => written(&s),
        None => cu.counted(),
    }
}

async fn events(home: &Home, a: EventsArgs, json: bool) -> Result<()> {
    let n = now();
    let since = a.since.as_deref().map(|s| parse_since(s, n)).transpose()?;
    let filter = EventFilter {
        since,
        run: a.run,
        group: a.group,
        kinds: a.kinds,
        limit: Some(a.limit),
        ..Default::default()
    };
    let evs = daemon::ensure(home, !json).await?.call::<EventList>(filter).await?;
    if json {
        println!("{}", serde_json::to_string(&evs)?);
    } else if evs.is_empty() {
        println!("no events");
    } else {
        print!(
            "{}",
            render::table(&render::EVENT_HEADERS, &render::event_rows(&evs, n))
        );
    }
    Ok(())
}

async fn run_daemon(home: &Home, cmd: DaemonCmd, json: bool) -> Result<()> {
    match cmd {
        DaemonCmd::Start => {
            let (info, started) = daemon::start(home).await?;
            if json {
                println!("{}", json!({ "started": started, "daemon": info }));
            } else if started {
                println!("rld started · {}", describe(&info));
            } else {
                println!("rld already running · {}", describe(&info));
            }
        }
        DaemonCmd::Stop { with_runs } => {
            let runs = if with_runs {
                stop_every_run(home, json).await?
            } else {
                0
            };
            let stopped = daemon::stop(home).await?;
            if json {
                println!("{}", json!({ "stopped": stopped, "runs_stopped": runs }));
            } else if stopped {
                let also = match runs {
                    0 => String::new(),
                    n => format!(" · {n} run{} stopped", if n == 1 { "" } else { "s" }),
                };
                println!("rld stopped{also}");
            } else {
                println!("rld was not running");
            }
        }
        DaemonCmd::Status => match daemon::status(home).await? {
            Some(info) => {
                if json {
                    println!("{}", json!({ "running": true, "daemon": info }));
                } else {
                    println!("rld running · {}", describe(&info));
                }
            }
            None => {
                if json {
                    println!("{}", json!({ "running": false, "home": home.path() }));
                } else {
                    println!("rld not running · home {}", display_path(home.path()));
                }
                std::process::exit(1);
            }
        },
        DaemonCmd::Restart => {
            let info = daemon::restart(home).await?;
            if json {
                println!("{}", json!({ "daemon": info }));
            } else {
                println!("rld restarted · {}", describe(&info));
            }
        }
    }
    Ok(())
}

fn describe(i: &DaemonInfo) -> String {
    format!(
        "pid {} · v{} · protocol {} · since {} · home {}",
        i.pid,
        i.version,
        i.protocol,
        i.started_at,
        display_path(&i.home)
    )
}

/// Emit a custom event. Inside a run, `RELAY_RUN` attributes it to that run's
/// folder and chain; outside one, the folder is `--in` or the cwd.
async fn emit(home: &Home, a: EmitArgs, json: bool) -> Result<()> {
    let mut data = serde_json::Map::new();
    if let Some(src) = &a.json_data {
        let text = if src == "-" {
            std::io::read_to_string(std::io::stdin())?
        } else {
            src.clone()
        };
        match serde_json::from_str::<serde_json::Value>(&text).context("reading --json")? {
            serde_json::Value::Object(m) => data.extend(m),
            _ => bail!("--json must be a JSON object"),
        }
    }
    for kv in &a.data {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow!("--data wants KEY=VALUE, got `{kv}`"))?;
        data.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    let run = a
        .run
        .or_else(|| std::env::var("RELAY_RUN").ok().filter(|s| !s.is_empty()));
    // A folder is needed only when no run says which one; then, here.
    let workspace = match a.in_ {
        Some(p) => Some(p),
        None if run.is_none() => Some(std::env::current_dir()?),
        None => None,
    };
    let params = EventEmitParams {
        kind: a.kind,
        run,
        workspace,
        group: a.group,
        summary: a.summary,
        data: (!data.is_empty()).then_some(serde_json::Value::Object(data)),
    };
    daemon::ensure(home, !json).await?.call::<EventEmit>(params).await?;
    Ok(())
}

/// `rl ask`: raise a question. With a title on the command line the flags say
/// it all; without one, the whole spec is YAML (or JSON) on stdin, which is
/// how an agent hands over options that carry actions.
async fn ask(home: &Home, a: AskArgs, json: bool) -> Result<()> {
    let mut spec = match &a.title {
        Some(t) => DecisionSpec {
            title: t.clone(),
            options: a
                .options
                .iter()
                .map(|o| match o.split_once(':') {
                    Some((k, l)) => DecisionOption::new(k.trim(), l.trim()),
                    None => DecisionOption::new(o.trim(), ""),
                })
                .collect(),
            ..Default::default()
        },
        None => {
            let text = std::io::read_to_string(std::io::stdin())?;
            if text.trim().is_empty() {
                bail!("say the question, or send its YAML on stdin");
            }
            spec_from_yaml(&text).context("reading the decision from stdin")?
        }
    };
    if let Some(s) = a.summary {
        spec.summary = Some(s);
    }
    if a.multi {
        spec.multi = true;
    }
    if a.group.is_some() {
        spec.group = a.group;
    }
    if a.expires.is_some() {
        spec.expires = a.expires;
    }
    if a.cwd.is_some() {
        spec.cwd = a.cwd;
    }
    if let Some(key) = &a.default_option {
        let mut found = false;
        for o in &mut spec.options {
            o.default = &o.key == key;
            found |= o.default;
        }
        if !found {
            bail!("--default {key}: no such option");
        }
    }
    // Inside a run, the question belongs to that run's chain.
    spec.run = a
        .run
        .or(spec.run)
        .or_else(|| std::env::var("RELAY_RUN").ok().filter(|s| !s.is_empty()));
    if spec.cwd.is_none() && spec.run.is_none() {
        spec.cwd = Some(std::env::current_dir()?);
    }

    let d = daemon::ensure(home, !json).await?.call::<DecisionRaise>(spec).await?;
    if json {
        println!("{}", serde_json::to_string(&d)?);
    } else {
        println!("{}", d.id);
    }
    Ok(())
}

async fn decisions(home: &Home, a: DecisionsArgs, json: bool) -> Result<()> {
    let list = daemon::ensure(home, !json)
        .await?
        .call::<DecisionList>(DecisionFilter {
            all: a.all,
            run: a.run,
            chain: None,
            limit: Some(a.limit),
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&list)?);
    } else if list.is_empty() {
        println!("nothing waiting on you");
    } else {
        let now = now();
        let rows: Vec<Vec<String>> = list.iter().map(|d| decision_row(d, now)).collect();
        println!("{}", render::table(DECISION_HEADERS, &rows));
    }
    Ok(())
}

const DECISION_HEADERS: &[&str] = &["ID", "AGE", "STATE", "TITLE", "OPTIONS"];

fn decision_row(d: &Decision, now: jiff::Timestamp) -> Vec<String> {
    let state = match d.status {
        DecisionStatus::Open => match d.expires_at {
            Some(e) => format!("open · {}", until(e, now)),
            None => "open".into(),
        },
        other => other.to_string(),
    };
    let options = if d.status.is_open() {
        d.options
            .iter()
            .map(|o| {
                if o.default {
                    format!("{}*", o.key)
                } else {
                    o.key.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else if d.chosen.is_empty() {
        String::new()
    } else {
        format!("→ {}", d.chosen.join(", "))
    };
    vec![
        d.id.clone(),
        rl_core::time::fmt_age(d.created_at, now),
        state,
        d.title.clone(),
        options,
    ]
}

async fn decide(home: &Home, a: DecideArgs, json: bool) -> Result<()> {
    let mut rpc = daemon::ensure(home, !json).await?;
    let d = if a.dismiss {
        if !a.options.is_empty() {
            bail!("--dismiss takes no options");
        }
        rpc.call::<DecisionDismiss>(DecisionNote { id: a.id, note: a.note })
            .await?
    } else {
        if a.options.is_empty() {
            // Nothing said: show the question and its options rather than guess.
            let d = rpc.call::<DecisionGet>(DecisionId { id: a.id }).await?;
            print!("{}", decision_text(&d, now()));
            return Ok(());
        }
        rpc.call::<DecisionTake>(DecisionTakeParams {
            id: a.id,
            options: a.options,
            note: a.note,
        })
        .await?
    };
    if json {
        println!("{}", serde_json::to_string(&d)?);
    } else {
        println!("{}", render::table(DECISION_HEADERS, &[decision_row(&d, now())]));
        for r in &d.outcome_runs {
            println!("started {r}");
        }
    }
    Ok(())
}

/// One question in full: what it asks, and what each option would do.
fn decision_text(d: &Decision, now: jiff::Timestamp) -> String {
    let mut out = format!("{}  {}\n", d.id, d.title);
    if let Some(s) = &d.summary {
        for line in s.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push('\n');
    for o in &d.options {
        let action = o.action().label();
        let mark = if o.default { "*" } else { " " };
        out.push_str(&format!("  {mark}{:<10} {}\n", o.key, o.text()));
        if !action.is_empty() {
            out.push_str(&format!("   {:<10} → {action}\n", ""));
        }
    }
    out.push_str(&format!("\nasked {} ago", rl_core::time::fmt_age(d.created_at, now)));
    match d.expires_at {
        Some(e) => out.push_str(&format!(" · expires {}\n", until(e, now))),
        None => out.push('\n'),
    }
    out.push_str(&format!("rl decide {} <option>\n", d.id));
    out
}

/// YAML (or JSON) → a decision. It goes through JSON on the way because
/// serde's `flatten` — what lets an option carry its action inline — only
/// reads sequences correctly from JSON.
fn spec_from_yaml(text: &str) -> Result<DecisionSpec> {
    let yaml: serde_norway::Value = serde_norway::from_str(text)?;
    Ok(serde_json::from_value(serde_json::to_value(yaml)?)?)
}

/// `in 3h12m`, or `due` for a moment already gone.
fn until(t: jiff::Timestamp, now: jiff::Timestamp) -> String {
    if t > now {
        format!("in {}", rl_core::time::fmt_duration(t.duration_since(now)))
    } else {
        "due".to_string()
    }
}

/// `rl config`: the file as the daemon reads it, each assistant resolved to
/// what would run; `rl config edit` opens it first. A file that cannot be
/// used says why and exits 1.
async fn config(home: &Home, a: ConfigArgs, json: bool) -> Result<()> {
    if let Some(ConfigCmd::Edit) = a.cmd {
        let file = home.config_file();
        if !file.exists() {
            std::fs::write(
                &file,
                "# Your defaults. Keys are added as Relay grows; one it does not know is refused.\n\
                 # assistants:\n#   interpret: claude   # or { agent: claude, model: …, args: [...] }\n\
                 #   summary: claude\n",
            )?;
        }
        rl_cli::env::open_in_editor(&file, 1)?;
    }
    let r = daemon::ensure(home, !json).await?.call::<ConfigGet>(()).await?;
    if json {
        println!("{}", serde_json::to_string(&r)?);
    } else {
        let file = display_path(&r.file);
        println!("{file}{}", if r.present { "" } else { "  (absent: all defaults)" });
        for (job, x) in [("interpret", &r.interpret), ("summary", &r.summary)] {
            match x {
                Some(x) => println!(
                    "assistants.{job:<10} {}  ({}{})",
                    x.argv.join(" "),
                    x.source,
                    if x.available { "" } else { ", not on this machine" }
                ),
                None => println!("assistants.{job:<10} —  (no agent on this machine; see `rl agents`)"),
            }
        }
        match r.config.agents.as_slice() {
            [] => println!("agents                —  (unset: the panel offers every agent here)"),
            a => println!("agents                {}  (config; `rl setup` asks)", a.join(", ")),
        }
        match r.config.retention.as_ref().and_then(|x| x.journal.as_deref()) {
            Some(d) if rl_proto::config::is_never(d) => {
                println!("retention.journal     {d}  (config; no journal is ever dropped)")
            }
            Some(d) => println!("retention.journal     {d}  (config; `rl prune` runs it now, rld daily)"),
            None => println!(
                "retention.journal     {}  (default; the terminal bytes of runs ended longer ago go, `never` keeps them)",
                rl_proto::config::DEFAULT_RETENTION
            ),
        }
        match r.config.runs.reap_after.as_deref() {
            Some(s) => {
                println!("runs.reap_after      {s}  (config; with nobody attached that long, a run stops itself)")
            }
            None => println!(
                "runs.reap_after      {}  (default; `never` keeps a run whatever happens to rld)",
                rl_proto::config::DEFAULT_REAP
            ),
        }
        match r.config.runs.settle.as_deref() {
            Some(s) => println!("runs.settle          {s}  (config; a burst of output gathers this long)"),
            None => println!(
                "runs.settle          {}  (default; `off` reads as fast as a run writes)",
                rl_proto::config::DEFAULT_SETTLE
            ),
        }
        println!(
            "catch_up.after        {}  ({}; away longer than this, the panel opens on what happened; `off` only on /since)",
            r.catch_up_after,
            if r.config.catch_up.after.is_some() {
                "config"
            } else {
                "default"
            }
        );
        println!(
            "catch_up.summary      {}  ({}; `auto` writes the top when you come back after `after`, by assistants.summary; `ask` only on /summary)",
            r.catch_up_summary,
            if r.config.catch_up.summary.is_some() {
                "config"
            } else {
                "default"
            }
        );
        match r.config.workspaces.badges.as_slice() {
            [] => println!("workspaces.badges     —  (unset: a header shows the branch alone; gh-pr adds the open PR)"),
            b => println!(
                "workspaces.badges     {}  (config; ~/.relay/plugins first on the PATH)",
                b.join(", ")
            ),
        }
        for e in &r.errors {
            eprintln!("not usable: {e}");
        }
    }
    if r.errors.is_empty() {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// `rl daemon stop --with-runs`: every live run stopped before the daemon
/// goes, so a throwaway home leaves nothing behind. A run that will not stop
/// is reported and the rest still go.
async fn stop_every_run(home: &Home, json: bool) -> Result<usize> {
    let Some(mut rpc) = daemon::connect_if_running(home).await? else {
        return Ok(0);
    };
    let live: Vec<rl_proto::types::Run> = rpc
        .call::<RunList>(RunFilter {
            active_only: true,
            ..Default::default()
        })
        .await?;
    let mut stopped = 0;
    for run in live {
        match rpc
            .call::<RunStop>(RunStopParams {
                id: run.id.clone(),
                force: false,
                archive: false,
            })
            .await
        {
            Ok(_) => stopped += 1,
            Err(e) if !json => eprintln!("{}: {e:#}", run.title),
            Err(_) => {}
        }
        // A holder lingers after its child so a daemon that was away can
        // still read the exit. Nobody is coming: ask it to go.
        let socket = home.session_socket(&run.id);
        if let Ok((_, mut s)) = rl_session::client::Session::attach(&socket, false).await {
            let _ = s.close().await;
            // And stay until it hangs up. Dropping the connection here would
            // break the holder's own handshake writes mid-flight, its client
            // task would end on the write error, and the `Close` waiting in
            // the buffer would never be read — leaving the holder sitting on
            // its socket for the whole linger.
            let _ = tokio::time::timeout(GOING, async { while let Ok(Some(_)) = s.recv().await {} }).await;
        }
    }
    Ok(stopped)
}

/// `rl prune` (once `rl gc`): retention, now — the panel's `/prune`.
async fn prune(home: &Home, a: PruneArgs, json: bool) -> Result<()> {
    let r = daemon::ensure(home, !json)
        .await?
        .call::<GcRun>(GcParams {
            older_than: a.older_than,
            dry_run: a.dry_run,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&r)?);
        return Ok(());
    }
    let size = rl_daemon_bytes(r.bytes);
    let folders = match r.firings {
        0 => String::new(),
        1 => " · 1 firing's folder".to_string(),
        n => format!(" · {n} firings' folders"),
    };
    if r.runs == 0 && r.firings == 0 {
        println!("nothing with a journal ended more than {} ago", r.older_than);
    } else if r.dry_run {
        println!(
            "would drop the journal of {} run{} ended more than {} ago · {size} · {} file{}{folders}",
            r.runs,
            if r.runs == 1 { "" } else { "s" },
            r.older_than,
            r.files,
            if r.files == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "dropped the journal of {} run{} ended more than {} ago · {size} · {} file{}{folders} · the runs and their events stay",
            r.runs,
            if r.runs == 1 { "" } else { "s" },
            r.older_than,
            r.files,
            if r.files == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// `4.1 MB`, `312 kB`, `88 B`.
fn rl_daemon_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} kB", n >> 10),
        n => format!("{n} B"),
    }
}

/// `rl context`: the pack, with `rl --help` and the help of the commands a
/// draft becomes, rendered from this very binary, and what the daemon knows
/// about here.
async fn context(home: &Home, a: ContextArgs) -> Result<()> {
    let folder = match a.folder {
        Some(f) => f,
        None => std::env::current_dir()?,
    };
    let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
    let mut rpc = daemon::ensure(home, true).await?;
    let here = rl_cli::context::gather(&mut rpc, &folder).await?;
    print!(
        "{}",
        rl_cli::context::render(rl_cli::context::help(), &here, &Want::Any)
    );
    Ok(())
}

/// The command list, and the help of the commands a draft would become,
/// for the context pack. Every command's long help is ten times the rest
/// of the pack, and an interpreter drafts things, it does not drive the CLI.
fn help_for_the_pack() -> String {
    let mut help = String::new();
    let mut root = <Cli as clap::CommandFactory>::command();
    help.push_str(&root.render_long_help().to_string());
    for sub in root.get_subcommands_mut() {
        if matches!(sub.get_name(), "run" | "flow" | "ask") {
            help.push_str("\n---\n");
            help.push_str(&sub.render_help().to_string());
        }
        if sub.get_name() == "flow" {
            for inner in sub.get_subcommands_mut() {
                if matches!(inner.get_name(), "new" | "test" | "state") {
                    help.push_str("\n---\n");
                    help.push_str(&inner.render_help().to_string());
                }
            }
        }
    }
    help
}

/// `rl interpret "…"`: the words to `assistants.interpret`, the draft
/// printed as YAML; `--go` does what it says, as `enter` in the panel would.
async fn interpret(home: &Home, a: InterpretArgs, json: bool) -> Result<()> {
    let folder = match a.folder {
        Some(f) => f,
        None => std::env::current_dir()?,
    };
    let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
    let words = a.words.join(" ");
    if words.trim().is_empty() {
        bail!("say what: rl interpret \"run the tests in api\"");
    }
    daemon::ensure(home, !json).await?;
    if !json {
        eprintln!("asking…");
    }
    let (answer, out) = rl_cli::interpret::ask(home, &folder, &words, &Want::Any).await?;
    let draft = match answer {
        Answer::Run(draft) => draft,
        Answer::Flow(d) => return rl_cli::interpret::flow_drafted(home, d, &out, &words, a.go, json).await,
    };
    let form = rl_cli::interpret::form_of(&draft, &folder);
    if json {
        println!(
            "{}",
            json!({ "draft": draft.fields, "kind": draft.lines()[0].trim_start_matches("kind: "), "run": out.run, "agent": out.agent })
        );
    } else {
        for l in draft.lines() {
            println!("{l}");
        }
        println!("# by {} · run {}", out.agent, out.run);
    }
    if a.go {
        let done = rl_cli::interpret::apply(home, &form).await?;
        if !json {
            println!("{done}");
        }
    } else if !json {
        println!("# --go does it; or n / a in the panel");
    }
    Ok(())
}

/// What `rl summary` was asked of: `all`, a folder (a project), a
/// `flow:<name>`, a `firing:<id>`, else a run.
fn summary_target(what: &str) -> Result<SummaryTarget> {
    Ok(if what == "all" {
        SummaryTarget::All
    } else if let Some(name) = what.strip_prefix("flow:") {
        SummaryTarget::Flow { name: name.to_string() }
    } else if let Some(id) = what.strip_prefix("firing:") {
        SummaryTarget::Firing { id: id.to_string() }
    } else if Path::new(what).is_dir() {
        SummaryTarget::Project {
            workspace: std::fs::canonicalize(what)?,
        }
    } else {
        SummaryTarget::Run { id: what.to_string() }
    })
}

async fn summary(home: &Home, a: SummaryArgs, json: bool) -> Result<()> {
    let target = summary_target(&a.what)?;
    let mut rpc = daemon::ensure(home, !json).await?;
    let from = match a.since.as_deref() {
        Some(s) => Some(parse_since(s, now())?),
        // A project or everything has no "since you looked" of its own: the
        // last day, and presence is not touched.
        None if matches!(target, SummaryTarget::Project { .. } | SummaryTarget::All) => {
            Some(now() - SignedDuration::from_hours(24))
        }
        None => None,
    };
    let ask = |again: bool| SummaryParams {
        target: target.clone(),
        deep: a.deep,
        from,
        again,
        dry: false,
        timeout: None,
    };
    let kept = rpc.call::<SummaryGet>(ask(false)).await?;
    if !json && (a.again || kept.is_none_or(|k| k.stale)) {
        eprintln!("summarising…");
    }
    let s = rpc.call::<SummaryMake>(ask(a.again)).await?;
    if json {
        println!("{}", serde_json::to_string(&s)?);
    } else {
        println!("{}", s.text);
        let by = if s.by.is_empty() {
            String::new()
        } else {
            format!(" · its run {}", s.by)
        };
        println!(
            "# {}{by} · --again makes a new one",
            rl_cli::tui::summary_label(&s, now())
        );
    }
    Ok(())
}

const AGENT_HEADERS: &[&str] = &["AGENT", "BIN", "HEADLESS", "MODEL", "OFFERED"];

/// `rl agents`: what the registry knows, and what is here.
/// zsh: `D` with the last exit and `A` from `precmd`, `C` from `preexec`, and
/// `B` on the end of the prompt itself, where what you type begins. At each
/// prompt, too, the folder it is in (OSC 7), percent-encoded byte by byte.
const ZSH_MARKS: &str = r#"# Relay prompt marks (OSC 133) and the folder you are in (OSC 7), so a project
# shell can tell one command from the next and say where it is.
# In ~/.zshrc:  eval "$(rl shell-init zsh)"
if [[ -z ${_relay_marks:-} ]]; then
  _relay_marks=1
  autoload -Uz add-zsh-hook
  _relay_cwd() {
    local LC_ALL=C s=$PWD out= c i
    for (( i = 1; i <= ${#s}; i++ )); do
      c=${s[i]}
      case $c in
        [-/._~A-Za-z0-9]) out+=$c ;;
        *) out+=$(printf '%%%02X' "'$c") ;;
      esac
    done
    printf '\033]7;file://%s%s\007' "$HOST" "$out"
  }
  _relay_precmd() { printf '\033]133;D;%s\007\033]133;A\007' "$?"; _relay_cwd }
  _relay_preexec() { printf '\033]133;C\007' }
  add-zsh-hook precmd _relay_precmd
  add-zsh-hook preexec _relay_preexec
  PS1="$PS1"$'\033]133;B\007'
fi
"#;

/// bash: the same, with `PROMPT_COMMAND` for `D`/`A` and the folder — first,
/// so `$?` is still the command's — a `DEBUG` trap for `C`, and `B` on the end
/// of `PS1`.
const BASH_MARKS: &str = r#"# Relay prompt marks (OSC 133) and the folder you are in (OSC 7), so a project
# shell can tell one command from the next and say where it is.
# In ~/.bashrc:  eval "$(rl shell-init bash)"
if [ -z "${_relay_marks:-}" ]; then
  _relay_marks=1
  _relay_cwd() {
    local LC_ALL=C s=$PWD out= c i n
    for (( i = 0; i < ${#s}; i++ )); do
      c=${s:i:1}
      case $c in
        [-/._~A-Za-z0-9]) out+=$c ;;
        # A byte past ASCII reads negative in bash 3.2 (macOS's own bash).
        *) printf -v n '%d' "'$c"; printf -v c '%%%02X' $(( n & 255 )); out+=$c ;;
      esac
    done
    printf '\033]7;file://%s%s\007' "$HOSTNAME" "$out"
  }
  _relay_precmd() { printf '\033]133;D;%s\007\033]133;A\007' "$?"; _relay_cwd; }
  PROMPT_COMMAND="_relay_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  trap 'printf "\033]133;C\007"' DEBUG
  PS1="$PS1\[\033]133;B\007\]"
fi
"#;

/// The snippet for a shell, printed and never written anywhere: the marks are
/// handed over, like `RELAY_RUN`, for a prompt to adopt if it wants them.
/// Plenty of prompts — starship, powerlevel10k, the shell integrations of
/// iTerm2, WezTerm and kitty — already emit these, and then there is nothing
/// to add at all.
fn shell_init(a: ShellInitArgs) -> Result<()> {
    let shell = a
        .shell
        .or_else(|| {
            let sh = std::env::var("SHELL").ok()?;
            Path::new(&sh).file_name().map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let snippet = match shell.trim() {
        "zsh" => ZSH_MARKS,
        "bash" => BASH_MARKS,
        // Marking where input begins means wrapping fish_prompt itself, and
        // without that mark a command's text never arrives — better to say so
        // than to print something that quietly records nothing.
        "fish" => bail!("fish is not supported yet: its `B` mark needs fish_prompt wrapped"),
        "" => bail!("no shell given and $SHELL says nothing; try `rl shell-init zsh`"),
        other => bail!("no prompt marks for {other}; zsh and bash are known"),
    };
    print!("{snippet}");
    Ok(())
}

async fn agents(home: &Home, json: bool) -> Result<()> {
    let list = daemon::ensure(home, !json).await?.call::<AgentList>(()).await?;
    if json {
        println!("{}", serde_json::to_string(&list)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|a| {
            vec![
                a.name.clone(),
                a.bin
                    .as_ref()
                    .map(|p| display_path(p))
                    .unwrap_or_else(|| format!("— ({})", a.bins.join(", "))),
                a.headless.join(" "),
                if a.model_flag.is_empty() {
                    "—".into()
                } else {
                    format!("{} <model>", a.model_flag.join(" "))
                },
                if a.offered { "yes" } else { "no (config: agents)" }.into(),
            ]
        })
        .collect();
    print!("{}", render::table(AGENT_HEADERS, &rows));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn the_flow_state_commands_the_pack_teaches_exist() {
        let root = Cli::command();
        let state = root
            .find_subcommand("flow")
            .and_then(|f| f.find_subcommand("state"))
            .expect("rl flow state");
        for sub in ["set", "get", "write", "read"] {
            assert!(state.find_subcommand(sub).is_some(), "rl flow state {sub}");
        }
    }

    #[test]
    fn rl_hook_takes_any_words_through_clap_too() {
        // `rl --json hook …`, read by clap: it takes any words.
        match Cli::try_parse_from(["rl", "--json", "hook", "a", "--weird", "-x", "--help"]).map(|c| c.cmd) {
            Ok(Some(Cmd::Hook(a))) => assert_eq!(a.words, ["a", "--weird", "-x", "--help"]),
            other => panic!("{:?}", other.err()),
        }
    }

    #[test]
    fn the_shell_snippets_say_the_folder_percent_encoded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a b&c").join("café");
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        // `/bin/bash` too: on macOS it is bash 3.2, whatever `bash` finds first.
        for (sh, snippet) in [("bash", BASH_MARKS), ("/bin/bash", BASH_MARKS), ("zsh", ZSH_MARKS)] {
            let file = tmp.path().join(sh.trim_start_matches("/bin/"));
            std::fs::write(&file, snippet).unwrap();
            let script = format!(". '{}'; cd '{}' && _relay_cwd", file.display(), dir.display());
            let Ok(out) = std::process::Command::new(sh).args(["-c", &script]).output() else {
                eprintln!("skipping {sh}: not installed");
                continue;
            };
            assert!(out.status.success(), "{sh}: {}", String::from_utf8_lossy(&out.stderr));
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("a%20b%26c/caf%C3%A9\x07"), "{sh}: {text:?}");
            let mut s = rl_detect::ansi::Stripper::new();
            s.strip(&out.stdout);
            assert_eq!(s.take_cwd(), Some(dir.clone()), "{sh}");
        }
    }

    #[test]
    fn the_grammar_is_consistent() {
        // Catches clashes like a subcommand arg colliding with a global flag.
        Cli::command().debug_assert();
    }

    #[test]
    fn emit_parses_without_clashing_with_the_global_json() {
        let cli = Cli::try_parse_from([
            "rl",
            "emit",
            "log.issue",
            "-d",
            "severity=ERROR",
            "--json",
            "--json-data",
            "-",
        ])
        .expect("emit args parse");
        let Some(Cmd::Emit(a)) = cli.cmd else {
            panic!("expected emit")
        };
        assert_eq!(a.kind, "log.issue");
        assert_eq!(a.data, ["severity=ERROR"]);
        assert_eq!(a.json_data.as_deref(), Some("-"));
        assert!(cli.json, "the global --json still reads as a bool");
    }

    #[test]
    fn a_question_can_be_written_as_yaml() {
        let yaml = concat!(
            "title: 3 errors in prod\n",
            "group: nightly/errors\n",
            "expires: 4h\n",
            "options:\n",
            "  - key: fix\n",
            "    label: Open a PR\n",
            "    run:\n",
            "      agent: claude\n",
            "      prompt: fix the NPE in checkout\n",
            "  - key: ignore\n",
            "    label: Ignore\n",
            "    default: true\n",
        );
        let spec = spec_from_yaml(yaml).unwrap();
        assert_eq!(spec.title, "3 errors in prod");
        assert_eq!(spec.group.as_deref(), Some("nightly/errors"));
        assert_eq!(spec.expires.as_deref(), Some("4h"));
        assert_eq!(spec.options.len(), 2);
        assert_eq!(spec.options[0].action().label(), "claude · fix the NPE in checkout");
        assert!(spec.options[1].default);
        assert!(spec.options[1].action().is_empty());
    }

    #[test]
    fn an_option_can_carry_a_command() {
        let spec = spec_from_yaml("title: ship?\noptions:\n  - key: ship\n    run: [git, push]\n").unwrap();
        assert_eq!(spec.options[0].action().label(), "git push");
    }
}

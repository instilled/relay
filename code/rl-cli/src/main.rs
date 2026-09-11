use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use relay_core::time::{now, parse_since};
use relay_core::{Home, display_path};
use relay_proto::decision::{Decision, DecisionOption, DecisionStatus};
use relay_proto::methods::{AgentList, ConfigGet, RuleApprove, RuleList, RuleRef, RunArchive, RunArchiveParams};
use relay_proto::methods::{
    DaemonInfo, DigestGet, DigestParams, EventFilter, EventList, PresenceClient, PresenceTouch, RunFilter, RunHook,
    RunHookParams, RunList, RunMark, RunMarkParams, RunSpec, RunStart, RunStop, RunStopParams,
};
use relay_proto::methods::{
    DecisionDismiss, DecisionFilter, DecisionGet, DecisionId, DecisionList, DecisionNote, DecisionRaise, DecisionSpec,
    DecisionTake, DecisionTakeParams,
};
use relay_proto::methods::{
    EventId, ReactionList, ReactionPause, ReactionReload, ReactionResume, ReactionTest, ReactionTrial,
};
use relay_proto::methods::{
    ScheduleFilter, ScheduleList, ScheduleName, SchedulePause, ScheduleReload, ScheduleResume, ScheduleRun,
    WorkspaceList, WorkspacePath, WorkspaceRemove, WorkspaceUp, WorkspaceUpParams,
};
use relay_proto::reaction::Reaction;
use relay_proto::rule::{Rule, RuleKind, RuleStatus};
use relay_proto::schedule::{Schedule, UpReport};
use relay_proto::types::{AttachMode, Cause, Mark, RunKind, RunState};
use rl_cli::client::Client;
use rl_cli::daemon;
use rl_cli::env::child_env;
use rl_cli::render;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

/// How long `rl hook` gives the daemon before letting the CLI carry on.
const HOOK_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Parser)]
#[command(
    name = "rl",
    version,
    about = "Relay — keeps your agents and long-running work alive",
    after_help = "For agents:\n  rl context    what Relay is, its events and rules, and the answer it wants — read this first\n  rl agents     the agent CLIs on this machine\n  rl --json …   every command, machine-readable"
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
    /// Stop a run (SIGTERM; --force for SIGKILL).
    Stop(StopArgs),
    /// List runs (active by default).
    Ls(LsArgs),
    /// Done with a run: out of the panel and `rl since`, kept in the ledger. Ended runs only.
    Done(DoneArgs),
    /// What happened while you were away: runs that need you, finished, or moved, grouped.
    Since(SinceArgs),
    /// List ledger events.
    Events(EventsArgs),
    /// Runs that start on their own: what schedules.yaml says, and when.
    Schedule {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
    /// Rules that answer events: what reactions.yaml says, and whether they fire.
    React {
        #[command(subcommand)]
        cmd: ReactCmd,
    },
    /// What starts work without you: every schedule and reaction, one list. Approve, pause, run.
    Rules(RulesArgs),
    /// Write a rule into your reactions.yaml: `rl on run.failed -- notify "{summary}"`.
    On(OnArgs),
    /// Write a schedule into this folder's schedules.yaml and switch it on: `rl every 30m -- make test`.
    Every(EveryArgs),
    /// Your defaults (~/.relay/config.yaml) as they stand, each assistant resolved; `edit` opens the file.
    Config(ConfigArgs),
    /// The agent CLIs Relay knows, and which are on this machine.
    Agents,
    /// What Relay tells an agent: the words, events, rule definitions, what is here, and the answer it wants.
    Context(ContextArgs),
    /// Ask a human something and carry on: the question waits until answered.
    Ask(AskArgs),
    /// Questions waiting on you (and, with --all, the answered ones).
    Decisions(DecisionsArgs),
    /// Answer one: choose an option, or dismiss it.
    Decide(DecideArgs),
    /// Register this folder as a workspace and load its .relay/: schedules fire once approved.
    Up(UpArgs),
    /// Registered folders (see `rl up`).
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// See a run's terminal live, read-only. q to leave.
    Watch(AttachArgs),
    /// Be in a run's terminal: keyboard, resize, the lot. ctrl-\ to detach; the run keeps going.
    Join(AttachArgs),
    /// Say what a run is doing when its CLI cannot: done, blocked or working.
    Mark(MarkArgs),
    /// Called by a CLI's hooks (Claude Code) with the hook's JSON on stdin. Never fails the CLI.
    #[command(hide = true)]
    Hook(HookArgs),
}

#[derive(Args)]
struct AttachArgs {
    /// Run id.
    id: String,
    /// Join even if someone else has; they become a watcher.
    #[arg(long)]
    take: bool,
    /// KiB of scrollback to replay first.
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
    /// The hook event name, e.g. Notification or Stop.
    event: String,
    /// The CLI's hook vocabulary (default: claude).
    #[arg(long, default_value = "claude")]
    dialect: String,
}

#[derive(Subcommand)]
enum ScheduleCmd {
    /// List schedules: when they fire next, what they did last.
    Ls {
        /// Include schedules removed from the file.
        #[arg(short, long)]
        all: bool,
    },
    /// Fire a schedule now, whatever the clock or its policies say.
    Run(ScheduleRef),
    /// Stop a schedule from firing until resumed.
    Pause(ScheduleRef),
    /// Resume a paused schedule; it plans afresh from now.
    Resume(ScheduleRef),
    /// Re-read every schedules file (rld does it on start too).
    Reload,
}

#[derive(Args)]
struct ScheduleRef {
    name: String,
    /// Which one, when the name exists in several: `global` or a workspace folder.
    #[arg(long = "in", value_name = "SCOPE")]
    scope: Option<String>,
}

#[derive(Subcommand)]
enum ReactCmd {
    /// List the rules: what they watch for, and what they do.
    Ls {
        /// Include rules removed from the file.
        #[arg(short, long)]
        all: bool,
    },
    /// What every rule would do with one event, without doing any of it.
    Test {
        /// Event id, from `rl events`.
        event: i64,
    },
    /// Stop a rule from firing until resumed.
    Pause(ScheduleRef),
    /// Let it fire again.
    Resume(ScheduleRef),
    /// Re-read every reactions file (rld does it on start too).
    Reload,
}

#[derive(Args)]
struct RulesArgs {
    /// Include rules removed from their files.
    #[arg(short, long)]
    all: bool,
    #[command(subcommand)]
    cmd: Option<RulesCmd>,
}

#[derive(Subcommand)]
enum RulesCmd {
    /// Say yes to a rule's current version; it may fire from now on.
    Approve(RuleArg),
    /// Stop a rule from firing until resumed.
    Pause(RuleArg),
    /// Let it fire again.
    Resume(RuleArg),
    /// Fire a schedule now, whatever the clock says.
    Run(RuleArg),
    /// Re-read every schedules and reactions file.
    Reload,
}

#[derive(Args)]
struct RuleArg {
    name: String,
    /// Which one, when the name exists in several: `global` or a workspace folder.
    #[arg(long = "in", value_name = "SCOPE")]
    scope: Option<String>,
    /// `schedule` or `reaction`, when both share the name.
    #[arg(long)]
    kind: Option<RuleKind>,
}

#[derive(Args)]
struct OnArgs {
    /// The event kind, or a family: `run.failed`, `run.*`, `decision.raised`.
    kind: String,
    /// What to run. Everything after `--`; `{summary}`, `{title}`, `{run}` and
    /// the rest are filled in from the event.
    #[arg(last = true, required = true)]
    argv: Vec<String>,
    /// What to call it (default: from the kind).
    #[arg(long)]
    name: Option<String>,
    /// Only for runs carrying this tag.
    #[arg(long)]
    tag: Option<String>,
    /// Where it runs (default: here).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Not again within this long (default: 10s).
    #[arg(long)]
    throttle: Option<String>,
}

#[derive(Args)]
#[command(
    override_usage = "rl every <WHEN> [OPTIONS] -- <COMMAND>...\n       rl every <WHEN> --agent <CLI> [OPTIONS] -- <PROMPT>..."
)]
struct EveryArgs {
    /// How often: a duration (`30m`, `4h`) or a cron line (`"0 2 * * *"`).
    when: String,
    /// What to run. One word is a shell line; several are argv; with --agent, the prompt.
    #[arg(last = true, required = true)]
    argv: Vec<String>,
    /// Run an agent CLI (claude, codex, …) with the rest as its prompt.
    #[arg(long)]
    agent: Option<String>,
    /// What to call it (default: the command's first word).
    #[arg(long)]
    name: Option<String>,
    /// Which folder's rule it is (default: here).
    #[arg(long = "in", value_name = "FOLDER")]
    folder: Option<PathBuf>,
    /// One of yours, in the home's schedules.yaml, instead of a folder's.
    #[arg(long, conflicts_with = "folder")]
    global: bool,
    /// Where it runs (default: the folder; for --global, here).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Title of the runs it starts.
    #[arg(long)]
    title: Option<String>,
    /// A tag on its runs. Repeatable.
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// Rewrite a rule of that name instead of refusing.
    #[arg(long)]
    replace: bool,
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
    /// Approve every pending schedule without asking.
    #[arg(short, long)]
    yes: bool,
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// List registered folders.
    Ls,
    /// Unregister a folder; its schedules stop.
    Rm { dir: PathBuf },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start rld if it is not already running.
    Start,
    /// Ask rld to exit. Running sessions are untouched.
    Stop,
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
    /// Only runs with this cause, e.g. schedule:nightly.
    #[arg(long, value_name = "CAUSE")]
    cause: Option<String>,
    /// Newest N.
    #[arg(short = 'n', long, default_value_t = 200)]
    limit: u32,
}

#[derive(Args)]
struct DoneArgs {
    /// Run ids.
    ids: Vec<String>,
    /// Instead: everything that has ended and been looked at.
    #[arg(long, conflicts_with = "ids")]
    all: bool,
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
    let cli = Cli::parse();
    let home = Home::resolve()?;
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
        Cmd::Done(a) => done(&home, a, cli.json).await,
        Cmd::Since(a) => since(&home, a, cli.json).await,
        Cmd::Events(a) => events(&home, a, cli.json).await,
        Cmd::Schedule { cmd } => schedule(&home, cmd, cli.json).await,
        Cmd::React { cmd } => react(&home, cmd, cli.json).await,
        Cmd::Rules(a) => rules(&home, a, cli.json).await,
        Cmd::On(a) => on(&home, a, cli.json).await,
        Cmd::Every(a) => every(&home, a, cli.json).await,
        Cmd::Config(a) => config(&home, a, cli.json).await,
        Cmd::Agents => agents(&home, cli.json).await,
        Cmd::Context(a) => context(&home, a).await,
        Cmd::Ask(a) => ask(&home, a, cli.json).await,
        Cmd::Decisions(a) => decisions(&home, a, cli.json).await,
        Cmd::Decide(a) => decide(&home, a, cli.json).await,
        Cmd::Up(a) => up(&home, a, cli.json).await,
        Cmd::Workspace { cmd } => workspace(&home, cmd, cli.json).await,
        Cmd::Watch(a) => attach(&home, a, AttachMode::Watch).await,
        Cmd::Join(a) => attach(&home, a, AttachMode::Join).await,
        Cmd::Mark(a) => mark(&home, a, cli.json).await,
        Cmd::Hook(a) => hook(&home, a).await,
    }
}

async fn attach(home: &Home, a: AttachArgs, mode: AttachMode) -> Result<()> {
    let code = rl_cli::attach::attach(
        home,
        rl_cli::attach::Opts {
            id: a.id,
            mode,
            take: a.take,
            scrollback: a.scrollback * 1024,
        },
    )
    .await?;
    std::process::exit(code)
}

async fn mark(home: &Home, a: MarkArgs, json: bool) -> Result<()> {
    let run = Client::connect(home)
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

/// A hook must never slow down or break the CLI that called it: no run id
/// means we are not under Relay, and any daemon trouble is swallowed. Nothing
/// is printed — some CLIs feed hook stdout back to the model.
async fn hook(home: &Home, a: HookArgs) -> Result<()> {
    let Some(id) = std::env::var("RELAY_RUN").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    // The CLI writes the payload and closes the pipe; reading it is quick.
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let payload = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let params = RunHookParams {
        id,
        dialect: a.dialect,
        event: a.event,
        payload,
    };
    let post = async {
        Client::connect(home).await?.call::<RunHook>(params).await?;
        anyhow::Ok(())
    };
    if let Ok(Err(e)) = tokio::time::timeout(HOOK_TIMEOUT, post).await
        && std::env::var_os("RELAY_HOOK_DEBUG").is_some()
    {
        eprintln!("rl hook: {e:#}");
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
        cause: Cause::Manual,
        flow: None,
    };
    let run = Client::connect(home).await?.call::<RunStart>(spec).await?;
    if json {
        println!("{}", serde_json::to_string(&run)?);
    } else {
        println!("{}", run.id);
    }
    Ok(())
}

async fn schedule(home: &Home, cmd: ScheduleCmd, json: bool) -> Result<()> {
    let mut rpc = Client::connect(home).await?;
    let name = |r: &ScheduleRef| ScheduleName {
        name: r.name.clone(),
        scope: r.scope.clone(),
    };
    let one = |s: Schedule, json: bool| -> Result<()> {
        if json {
            println!("{}", serde_json::to_string(&s)?);
        } else {
            println!("{}", render::table(SCHEDULE_HEADERS, &[schedule_row(&s, now())]));
        }
        Ok(())
    };
    match cmd {
        ScheduleCmd::Ls { all } => {
            let list = rpc.call::<ScheduleList>(ScheduleFilter { all }).await?;
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!("no schedules ({})", display_path(&home.schedules_file()));
            } else {
                let now = now();
                let rows: Vec<Vec<String>> = list.iter().map(|s| schedule_row(s, now)).collect();
                println!("{}", render::table(SCHEDULE_HEADERS, &rows));
            }
        }
        ScheduleCmd::Run(r) => {
            let run = rpc.call::<ScheduleRun>(name(&r)).await?;
            if json {
                println!("{}", serde_json::to_string(&run)?);
            } else {
                println!("{}", run.id);
            }
        }
        ScheduleCmd::Pause(r) => one(rpc.call::<SchedulePause>(name(&r)).await?, json)?,
        ScheduleCmd::Resume(r) => one(rpc.call::<ScheduleResume>(name(&r)).await?, json)?,
        ScheduleCmd::Reload => {
            let r = rpc.call::<ScheduleReload>(()).await?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!(
                    "{} loaded · {} added · {} changed · {} removed",
                    r.loaded,
                    r.added.len(),
                    r.changed.len(),
                    r.removed.len()
                );
                for e in &r.errors {
                    eprintln!("not loaded: {e}");
                }
            }
        }
    }
    Ok(())
}

const SCHEDULE_HEADERS: &[&str] = &["SCOPE", "NAME", "WHEN", "NEXT", "LAST", "STATE", "ACTION"];

async fn up(home: &Home, a: UpArgs, json: bool) -> Result<()> {
    let path = match a.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let mut rpc = Client::connect(home).await?;
    let mut r = rpc
        .call::<WorkspaceUp>(WorkspaceUpParams {
            path: path.clone(),
            approve: a.yes,
        })
        .await?;
    let pending: Vec<String> = r
        .schedules
        .iter()
        .filter(|s| !s.approved())
        .map(|s| s.name.clone())
        .collect();
    if !pending.is_empty() && !json && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        print_up(&r);
        eprint!("approve {} ({})? [y/N] ", pending.len(), pending.join(", "));
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().eq_ignore_ascii_case("y") {
            r = rpc
                .call::<WorkspaceUp>(WorkspaceUpParams {
                    path: path.clone(),
                    approve: true,
                })
                .await?;
        }
    }
    if json {
        println!("{}", serde_json::to_string(&r)?);
        return Ok(());
    }
    print_up(&r);
    if !r.approved.is_empty() {
        println!("approved: {}", r.approved.join(", "));
    } else if r.schedules.iter().any(|s| !s.approved()) {
        println!("not approved; nothing fires until `rl up --yes`");
    }
    for e in &r.report.errors {
        eprintln!("not loaded: {e}");
    }
    Ok(())
}

fn print_up(r: &UpReport) {
    println!("workspace {} ({})", r.workspace.name, display_path(&r.workspace.path));
    if r.schedules.is_empty() {
        println!("no schedules in .relay/schedules.yaml");
    } else {
        let now = now();
        let rows: Vec<Vec<String>> = r.schedules.iter().map(|s| schedule_row(s, now)).collect();
        println!("{}", render::table(SCHEDULE_HEADERS, &rows));
    }
}

async fn workspace(home: &Home, cmd: WorkspaceCmd, json: bool) -> Result<()> {
    let mut rpc = Client::connect(home).await?;
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
                            format!("{} ago", relay_core::time::fmt_age(w.registered_at, now)),
                        ]
                    })
                    .collect();
                println!("{}", render::table(&["NAME", "PATH", "REGISTERED"], &rows));
            }
        }
        WorkspaceCmd::Rm { dir } => {
            let gone = rpc.call::<WorkspaceRemove>(WorkspacePath { path: dir.clone() }).await?;
            if json {
                println!("{}", json!({ "removed": gone }));
            } else if gone {
                println!("{} unregistered; its schedules stop", display_path(&dir));
            } else {
                println!("{} was not registered", display_path(&dir));
            }
        }
    }
    Ok(())
}

fn schedule_row(s: &Schedule, now: jiff::Timestamp) -> Vec<String> {
    let next = match (s.paused || !s.approved(), s.removed_at, s.next_at) {
        (true, _, _) | (_, Some(_), _) => "—".to_string(),
        (_, _, Some(t)) if t > now => format!("in {}", relay_core::time::fmt_duration(t.duration_since(now))),
        (_, _, Some(_)) => "due".to_string(),
        (_, _, None) => "—".to_string(),
    };
    let last = match (s.last_fired_at, &s.last_run) {
        (Some(t), Some(id)) => format!("{} ago · {id}", relay_core::time::fmt_age(t, now)),
        _ => "never".to_string(),
    };
    let state = if s.removed_at.is_some() {
        "removed"
    } else if !s.approved() {
        "needs approval"
    } else if s.paused {
        "paused"
    } else {
        "on"
    };
    let action = s.spec.action().label();
    let action: String = action.chars().take(48).collect();
    vec![
        s.scope_label(),
        s.name.clone(),
        s.spec.when(),
        next,
        last,
        state.to_string(),
        action,
    ]
}

async fn stop(home: &Home, a: StopArgs, json: bool) -> Result<()> {
    let run = Client::connect(home)
        .await?
        .call::<RunStop>(RunStopParams {
            id: a.id,
            force: a.force,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&run)?);
    } else {
        println!(
            "{} · stop requested ({})",
            run.id,
            if a.force { "SIGKILL" } else { "SIGTERM" }
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
    let runs = Client::connect(home).await?.call::<RunList>(filter).await?;
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

async fn done(home: &Home, a: DoneArgs, json: bool) -> Result<()> {
    if a.ids.is_empty() && !a.all {
        bail!("say which: `rl done <id>…`, or `rl done --all` for everything ended and seen");
    }
    let n = Client::connect(home)
        .await?
        .call::<RunArchive>(RunArchiveParams {
            ids: a.ids.clone(),
            all_seen: a.all,
        })
        .await?;
    if json {
        println!("{}", json!({ "archived": n }));
    } else if n == 0 && !a.all {
        bail!("nothing archived: not found, still running, or already done");
    } else {
        println!("{n} done");
    }
    Ok(())
}

async fn since(home: &Home, a: SinceArgs, json: bool) -> Result<()> {
    let n = now();
    let mut c = Client::connect(home).await?;
    let boundary = match a.since.as_deref() {
        Some(s) => parse_since(s, n)?,
        None => {
            // Looking now; the boundary is the last time we did.
            let me = rl_cli::attach::client_name();
            c.call::<PresenceTouch>(PresenceClient { client: me })
                .await?
                .last_seen_at
                .unwrap_or(n - jiff::SignedDuration::from_hours(24))
        }
    };
    let d = c.call::<DigestGet>(DigestParams { since: Some(boundary) }).await?;
    if json {
        println!("{}", serde_json::to_string(&d)?);
    } else {
        print!("{}", rl_cli::digest::text(&d, n));
    }
    Ok(())
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
    };
    let evs = Client::connect(home).await?.call::<EventList>(filter).await?;
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
        DaemonCmd::Stop => {
            let stopped = daemon::stop(home).await?;
            if json {
                println!("{}", json!({ "stopped": stopped }));
            } else if stopped {
                println!("rld stopped");
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

    let d = Client::connect(home).await?.call::<DecisionRaise>(spec).await?;
    if json {
        println!("{}", serde_json::to_string(&d)?);
    } else {
        println!("{}", d.id);
    }
    Ok(())
}

async fn decisions(home: &Home, a: DecisionsArgs, json: bool) -> Result<()> {
    let list = Client::connect(home)
        .await?
        .call::<DecisionList>(DecisionFilter {
            all: a.all,
            run: a.run,
            flow: None,
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
        relay_core::time::fmt_age(d.created_at, now),
        state,
        d.title.clone(),
        options,
    ]
}

async fn decide(home: &Home, a: DecideArgs, json: bool) -> Result<()> {
    let mut rpc = Client::connect(home).await?;
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
    out.push_str(&format!("\nasked {} ago", relay_core::time::fmt_age(d.created_at, now)));
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
        format!("in {}", relay_core::time::fmt_duration(t.duration_since(now)))
    } else {
        "due".to_string()
    }
}

const RULE_HEADERS: &[&str] = &["SCOPE", "NAME", "WHEN", "STATUS", "NEXT", "LAST", "DOES"];

fn rule_row(r: &Rule, now: jiff::Timestamp) -> Vec<String> {
    let next = match (r.status(), r.next_at()) {
        (RuleStatus::On, Some(t)) if t > now => format!("in {}", relay_core::time::fmt_duration(t.duration_since(now))),
        (RuleStatus::On, Some(_)) => "due".to_string(),
        _ => "—".to_string(),
    };
    let last = match r.last_fired_at() {
        Some(t) => format!("{} ago", relay_core::time::fmt_age(t, now)),
        None => "never".to_string(),
    };
    let does: String = r.does().chars().take(48).collect();
    vec![
        r.scope_label(),
        r.name().to_string(),
        r.when(),
        r.status().to_string(),
        next,
        last,
        does,
    ]
}

/// Which kind a name is, when the caller did not say: the one that has it.
async fn rule_kind(rpc: &mut Client, a: &RuleArg) -> Result<RuleKind> {
    if let Some(k) = a.kind {
        return Ok(k);
    }
    let all = rpc.call::<RuleList>(ScheduleFilter { all: false }).await?;
    let scope_ok = |r: &Rule| match &a.scope {
        None => true,
        Some(s) => {
            r.scope() == s
                || r.scope_label() == *s
                || std::fs::canonicalize(s).is_ok_and(|p| p.to_string_lossy() == r.scope())
        }
    };
    let mut kinds: Vec<RuleKind> = all
        .iter()
        .filter(|r| r.name() == a.name && scope_ok(r))
        .map(Rule::kind)
        .collect();
    kinds.dedup();
    match kinds.as_slice() {
        [k] => Ok(*k),
        [] => bail!("no rule named `{}`", a.name),
        _ => bail!("`{}` is both a schedule and a reaction; say which with --kind", a.name),
    }
}

async fn rules(home: &Home, a: RulesArgs, json: bool) -> Result<()> {
    let mut rpc = Client::connect(home).await?;
    let one = |r: Rule, json: bool| -> Result<()> {
        if json {
            println!("{}", serde_json::to_string(&r)?);
        } else {
            println!("{}", render::table(RULE_HEADERS, &[rule_row(&r, now())]));
        }
        Ok(())
    };
    let name = |x: &RuleArg| ScheduleName {
        name: x.name.clone(),
        scope: x.scope.clone(),
    };
    match a.cmd {
        None => {
            let mut list = rpc.call::<RuleList>(ScheduleFilter { all: a.all }).await?;
            list.sort_by(|x, y| {
                x.is_global()
                    .cmp(&y.is_global())
                    .reverse()
                    .then_with(|| x.scope_label().cmp(&y.scope_label()))
                    .then_with(|| x.name().cmp(y.name()))
            });
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!(
                    "no rules; `rl on` writes one, `rl up` loads a folder's ({}, {})",
                    display_path(&home.schedules_file()),
                    display_path(&home.reactions_file())
                );
            } else {
                let now = now();
                let rows: Vec<Vec<String>> = list.iter().map(|r| rule_row(r, now)).collect();
                print!("{}", render::table(RULE_HEADERS, &rows));
                let pending = list.iter().filter(|r| r.status() == RuleStatus::NeedsApproval).count();
                if pending > 0 {
                    println!(
                        "\n{pending} need{} your yes: `rl rules approve <name>`",
                        if pending == 1 { "s" } else { "" }
                    );
                }
            }
        }
        Some(RulesCmd::Approve(x)) => {
            let r = rpc
                .call::<RuleApprove>(RuleRef {
                    name: x.name,
                    scope: x.scope,
                    kind: x.kind,
                })
                .await?;
            one(r, json)?;
        }
        Some(cmd @ (RulesCmd::Pause(_) | RulesCmd::Resume(_))) => {
            let pause = matches!(cmd, RulesCmd::Pause(_));
            let (RulesCmd::Pause(x) | RulesCmd::Resume(x)) = cmd else {
                unreachable!()
            };
            let r = match (rule_kind(&mut rpc, &x).await?, pause) {
                (RuleKind::Schedule, true) => Rule::Schedule(rpc.call::<SchedulePause>(name(&x)).await?),
                (RuleKind::Schedule, false) => Rule::Schedule(rpc.call::<ScheduleResume>(name(&x)).await?),
                (RuleKind::Reaction, true) => Rule::Reaction(rpc.call::<ReactionPause>(name(&x)).await?),
                (RuleKind::Reaction, false) => Rule::Reaction(rpc.call::<ReactionResume>(name(&x)).await?),
            };
            one(r, json)?;
        }
        Some(RulesCmd::Run(x)) => {
            let run = rpc.call::<ScheduleRun>(name(&x)).await?;
            if json {
                println!("{}", serde_json::to_string(&run)?);
            } else {
                println!("started {} · {}", run.id, run.title);
            }
        }
        Some(RulesCmd::Reload) => {
            let s = rpc.call::<ScheduleReload>(()).await?;
            let r = rpc.call::<ReactionReload>(()).await?;
            if json {
                println!("{}", json!({ "schedules": s, "reactions": r }));
            } else {
                println!("schedules: {}", reload_line(&s));
                println!("reactions: {}", reload_line(&r));
                for e in s.errors.iter().chain(r.errors.iter()) {
                    eprintln!("not loaded: {e}");
                }
            }
        }
    }
    Ok(())
}

fn reload_line(r: &relay_proto::schedule::ReloadReport) -> String {
    format!(
        "{} loaded · {} added · {} changed · {} removed",
        r.loaded,
        r.added.len(),
        r.changed.len(),
        r.removed.len()
    )
}

const REACTION_HEADERS: &[&str] = &["SCOPE", "NAME", "ON", "LAST", "STATE", "DOES"];

fn reaction_row(r: &Reaction, now: jiff::Timestamp) -> Vec<String> {
    let last = match (r.last_fired_at, &r.last_run) {
        (Some(t), Some(id)) => format!("{} ago · {id}", relay_core::time::fmt_age(t, now)),
        (Some(t), None) => format!("{} ago", relay_core::time::fmt_age(t, now)),
        _ => "never".to_string(),
    };
    let state = if r.removed_at.is_some() {
        "removed"
    } else if !r.approved() {
        "needs approval"
    } else if r.paused {
        "paused"
    } else {
        "on"
    };
    let does = match &r.spec.ask {
        Some(ask) => format!("asks: {}", ask.title),
        None => r.spec.action().label(),
    };
    let does: String = does.chars().take(48).collect();
    vec![
        r.scope_label(),
        r.name.clone(),
        r.spec.when(),
        last,
        state.to_string(),
        does,
    ]
}

async fn react(home: &Home, cmd: ReactCmd, json: bool) -> Result<()> {
    let mut rpc = Client::connect(home).await?;
    let one = |r: Reaction, json: bool| -> Result<()> {
        if json {
            println!("{}", serde_json::to_string(&r)?);
        } else {
            println!("{}", render::table(REACTION_HEADERS, &[reaction_row(&r, now())]));
        }
        Ok(())
    };
    match cmd {
        ReactCmd::Ls { all } => {
            let list = rpc.call::<ReactionList>(ScheduleFilter { all }).await?;
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!("no reactions ({})", display_path(&home.reactions_file()));
            } else {
                let now = now();
                let rows: Vec<Vec<String>> = list.iter().map(|r| reaction_row(r, now)).collect();
                println!("{}", render::table(REACTION_HEADERS, &rows));
            }
        }
        ReactCmd::Test { event } => {
            let trials = rpc.call::<ReactionTest>(EventId { event }).await?;
            if json {
                println!("{}", serde_json::to_string(&trials)?);
            } else if trials.is_empty() {
                println!("no reactions");
            } else {
                println!("{}", render::table(TRIAL_HEADERS, &trial_rows(&trials)));
            }
        }
        ReactCmd::Pause(r) => one(
            rpc.call::<ReactionPause>(ScheduleName {
                name: r.name,
                scope: r.scope,
            })
            .await?,
            json,
        )?,
        ReactCmd::Resume(r) => one(
            rpc.call::<ReactionResume>(ScheduleName {
                name: r.name,
                scope: r.scope,
            })
            .await?,
            json,
        )?,
        ReactCmd::Reload => {
            let r = rpc.call::<ReactionReload>(()).await?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!(
                    "{} loaded · {} added · {} changed · {} removed",
                    r.loaded,
                    r.added.len(),
                    r.changed.len(),
                    r.removed.len()
                );
                for e in &r.errors {
                    eprintln!("not loaded: {e}");
                }
            }
        }
    }
    Ok(())
}

const TRIAL_HEADERS: &[&str] = &["SCOPE", "NAME", "FIRES", "WHY", "WOULD"];

fn trial_rows(trials: &[ReactionTrial]) -> Vec<Vec<String>> {
    trials
        .iter()
        .map(|t| {
            vec![
                t.scope.clone(),
                t.reaction.clone(),
                if t.fires { "yes" } else { "no" }.to_string(),
                t.why.clone(),
                t.action.clone().unwrap_or_default().chars().take(48).collect(),
            ]
        })
        .collect()
}

/// `rl on`: append a rule to the user's own reactions.yaml. The file stays
/// yours — this only adds to the end of it, comments and all.
async fn on(home: &Home, a: OnArgs, json: bool) -> Result<()> {
    let name = match a.name {
        Some(n) => n,
        None => {
            let base: String = a
                .kind
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            format!("on-{}", base.trim_matches('-'))
        }
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        bail!("a name is letters, digits, `.`, `_` and `-`");
    }
    let cwd = match a.cwd {
        Some(c) => c,
        None => std::env::current_dir()?,
    };
    let file = home.reactions_file();
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if existing.contains(&format!("\n  {name}:")) || existing.starts_with(&format!("  {name}:")) {
        bail!("`{name}` is already in {}; edit it there", display_path(&file));
    }

    let mut block = String::new();
    if !existing.lines().any(|l| l.trim_start().starts_with("reactions:")) {
        block.push_str("reactions:\n");
    } else if !existing.ends_with('\n') {
        block.push('\n');
    }
    block.push_str(&format!("  {name}:\n    on: {}\n", yaml_scalar(&a.kind)));
    if let Some(tag) = &a.tag {
        block.push_str(&format!("    where: {{ tag: {} }}\n", yaml_scalar(tag)));
    }
    let argv: Vec<String> = a.argv.iter().map(|w| yaml_scalar(w)).collect();
    block.push_str(&format!("    run: [{}]\n", argv.join(", ")));
    block.push_str(&format!("    cwd: {}\n", yaml_scalar(&cwd.to_string_lossy())));
    if let Some(t) = &a.throttle {
        block.push_str(&format!("    throttle: {}\n", yaml_scalar(t)));
    }

    std::fs::write(&file, format!("{existing}{block}"))?;
    let report = Client::connect(home).await?.call::<ReactionReload>(()).await?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{block}");
        println!("→ {}", display_path(&file));
        for e in &report.errors {
            eprintln!("not loaded: {e}");
        }
    }
    Ok(())
}

/// `rl every`: a schedule into this folder's (or your own) schedules.yaml,
/// loaded and on — the command line is the yes. The file's comments and
/// other rules stay as they were.
async fn every(home: &Home, a: EveryArgs, json: bool) -> Result<()> {
    use relay_proto::action::Task;
    use relay_proto::schedule::ScheduleSpec;
    let mut spec = ScheduleSpec::default();
    let when = a.when.trim();
    if relay_core::time::parse_duration(when).is_some() {
        spec.every = Some(when.to_string());
    } else if when.split_whitespace().count() == 5 {
        spec.cron = Some(when.to_string());
    } else {
        bail!("`{when}` is neither a duration (30m, 4h) nor a cron line (\"0 2 * * *\")");
    }
    spec.run = Some(match a.agent {
        Some(agent) => Task::Agent {
            agent,
            prompt: Some(a.argv.join(" ")),
            prompt_file: None,
            args: vec![],
        },
        None if a.argv.len() == 1 => Task::Shell(a.argv[0].clone()),
        None => Task::Argv(a.argv.clone()),
    });
    let here = std::env::current_dir()?;
    let folder = if a.global {
        None
    } else {
        let f = a.folder.unwrap_or_else(|| here.clone());
        Some(std::fs::canonicalize(&f).with_context(|| format!("{}: not a folder", f.display()))?)
    };
    spec.cwd = match (a.cwd, &folder) {
        (Some(c), _) => Some(c),
        (None, None) => Some(here),
        (None, Some(_)) => None,
    };
    spec.title = a.title;
    spec.tags = a.tags;
    let name = match a.name {
        Some(n) => n,
        None => {
            let first = match spec.run.as_ref().unwrap() {
                Task::Agent { agent, .. } => agent.clone(),
                Task::Shell(line) => line.split_whitespace().next().unwrap_or_default().to_string(),
                Task::Argv(v) => v[0].clone(),
            };
            let base = std::path::Path::new(&first)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(first);
            base.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || "._-".contains(c) {
                        c
                    } else {
                        '-'
                    }
                })
                .collect()
        }
    };
    rl_cli::rulefile::check_name(&name)?;
    let d = rl_cli::rulefile::RuleDraft {
        kind: RuleKind::Schedule,
        folder,
        name,
        body: rl_cli::rulefile::body(&spec, &ScheduleSpec::default())?,
    };
    let file = rl_cli::rulefile::file(home, d.kind, d.folder.as_deref());
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if !a.replace && rl_cli::rulefile::line_of(&text, "schedules", &d.name).is_some() {
        bail!(
            "`{}` is already in {}; --replace to rewrite it, or --name for another",
            d.name,
            display_path(&file)
        );
    }
    let mut rpc = Client::connect(home).await?;
    let registered = match &d.folder {
        Some(f) => rpc.call::<WorkspaceList>(()).await?.iter().any(|w| &w.path == f),
        None => true,
    };
    let file = rl_cli::rulefile::install(home, &mut rpc, &d, None, registered).await?;
    let rule = rpc
        .call::<RuleApprove>(RuleRef {
            name: d.name.clone(),
            scope: Some(d.scope()),
            kind: Some(RuleKind::Schedule),
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string(&rule)?);
    } else {
        println!("  {}:", d.name);
        for l in &d.body {
            println!("    {l}");
        }
        println!("→ {} · {}", display_path(&file), rule.status());
    }
    Ok(())
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
                 # assistants:\n#   interpret: claude   # or { agent: claude, model: …, args: [...] }\n",
            )?;
        }
        rl_cli::env::open_in_editor(&file, 1)?;
    }
    let r = Client::connect(home).await?.call::<ConfigGet>(()).await?;
    if json {
        println!("{}", serde_json::to_string(&r)?);
    } else {
        let file = display_path(&r.file);
        println!("{file}{}", if r.present { "" } else { "  (absent: all defaults)" });
        match &r.interpret {
            Some(x) => println!(
                "assistants.interpret  {}  ({}{})",
                x.argv.join(" "),
                x.source,
                if x.available { "" } else { ", not on this machine" }
            ),
            None => println!("assistants.interpret  —  (no agent on this machine; see `rl agents`)"),
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

/// `rl context`: the pack, with `rl --help` and the help of the commands a
/// draft becomes, rendered from this very binary, and what the daemon knows
/// about here.
async fn context(home: &Home, a: ContextArgs) -> Result<()> {
    let folder = match a.folder {
        Some(f) => f,
        None => std::env::current_dir()?,
    };
    let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
    let mut rpc = Client::connect(home).await?;
    let workspaces = rpc.call::<WorkspaceList>(()).await?;
    let rules = rpc.call::<RuleList>(ScheduleFilter { all: false }).await?;
    let agents = rpc.call::<AgentList>(()).await?;
    let name = workspaces.iter().find(|w| w.path == folder).map(|w| w.name.clone());
    let scope_word = |r: &Rule| -> String {
        if r.is_global() {
            "yours".into()
        } else {
            workspaces
                .iter()
                .find(|w| w.path.as_os_str() == r.scope())
                .map(|w| w.name.clone())
                .unwrap_or_else(|| display_path(std::path::Path::new(r.scope())))
        }
    };
    let here = rl_cli::context::Here {
        folder: display_path(&folder),
        name,
        folders: workspaces
            .iter()
            .map(|w| (display_path(&w.path), w.name.clone()))
            .collect(),
        rules: rules
            .iter()
            .filter(|r| r.is_global() || std::path::Path::new(r.scope()) == folder)
            .map(|r| {
                (
                    scope_word(r),
                    r.kind().to_string(),
                    r.name().to_string(),
                    r.when(),
                    r.does(),
                )
            })
            .collect(),
        agents: agents
            .iter()
            .filter(|a| a.available())
            .map(|a| a.name.clone())
            .collect(),
    };
    // The command list, and the help of the commands a draft would become.
    // Every command's long help is ten times the rest of the pack, and an
    // interpreter drafts things, it does not drive the CLI.
    let mut help = String::new();
    let mut root = <Cli as clap::CommandFactory>::command();
    help.push_str(&root.render_long_help().to_string());
    for sub in root.get_subcommands_mut() {
        if matches!(sub.get_name(), "run" | "every" | "on" | "ask") {
            help.push_str("\n---\n");
            help.push_str(&sub.render_help().to_string());
        }
    }
    print!("{}", rl_cli::context::render(&help, &here));
    Ok(())
}

const AGENT_HEADERS: &[&str] = &["AGENT", "BIN", "HEADLESS", "MODEL"];

/// `rl agents`: what the registry knows, and what is here.
async fn agents(home: &Home, json: bool) -> Result<()> {
    let list = Client::connect(home).await?.call::<AgentList>(()).await?;
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
            ]
        })
        .collect();
    print!("{}", render::table(AGENT_HEADERS, &rows));
    Ok(())
}

/// A YAML scalar that survives whatever the shell handed us.
fn yaml_scalar(s: &str) -> String {
    let plain =
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) && !s.starts_with('-');
    if plain {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

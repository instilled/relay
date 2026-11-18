use anyhow::Result;
use clap::{Parser, Subcommand};
use rl::{cmd, hook};
use rl_core::Home;
use std::path::PathBuf;

/// Relay: list, attach and run shells for any agent. With no command, the
/// list.
/// `rl --version`: the crate's version and the commit it was built from
/// (`build.rs`), for bug reports.
const VERSION: &str = env!("RL_VERSION");

#[derive(Parser)]
#[command(name = "rl", version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start a session and print its name.
    New {
        /// A harness profile (claude, codex, ...); the arguments after `--` go to it.
        #[arg(long)]
        harness: Option<String>,
        /// Where it runs (default: here).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Print the name only.
        #[arg(long)]
        name_only: bool,
        /// The command to run; empty starts a shell.
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// List sessions.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Attach this terminal to a session (ctrl+\ detaches).
    Attach { name: String },
    /// Close a running session.
    Stop { name: String },
    /// Remove a session that is not running.
    Rm { name: String },
    /// Set your own colour for a session in the list: a name (red, blue,
    /// green, yellow, purple, orange, pink, cyan), `#rrggbb`, or `none`.
    /// The session is a held one's name, or a row's key as the list shows it
    /// (`n:claude:<id>`, `o:claude:<session id>`).
    Color { session: String, colour: String },
    /// Serve send, who and inbox to an agent over stdio (an MCP server).
    Mcp {
        /// The harness that runs it (what `rl setup` registered it with).
        #[arg(long)]
        harness: String,
    },
    /// Install the hooks and register the MCP server with each harness. On
    /// a terminal, a walk through it; `--yes`, or no terminal, plain output.
    Setup {
        /// Take Relay's hooks and server out again.
        #[arg(long, conflicts_with = "check")]
        remove: bool,
        /// Change nothing: say what a run would change, one line per
        /// harness, and exit 1 when anything needs setting up.
        #[arg(long)]
        check: bool,
        /// Only this harness (repeatable); default: each one found on PATH.
        #[arg(long = "harness", value_name = "H")]
        harness: Vec<String>,
        /// Set up everything (but what you declined) without asking, and
        /// say what changed as plain lines.
        #[arg(long, conflicts_with_all = ["check", "remove"])]
        yes: bool,
    },
    /// Report a harness that changed under Relay as an issue on its
    /// GitHub repository: each is shown first, and posted only on your yes.
    Report {
        /// Show the issues; post nothing, ask nothing.
        #[arg(long)]
        dry_run: bool,
        /// Dismiss what there is to report, posting nothing.
        #[arg(long, conflicts_with = "dry_run")]
        dismiss: bool,
    },
    /// The harnesses found, their versions, and what drift Relay noticed.
    Doctor,
}

#[tokio::main]
async fn main() {
    rl::trace::born();
    // `rl hook` is read before clap reads anything: a hook never fails the
    // agent that called it, nor prints.
    if let Some(words) = hook::args() {
        if let (Some(words), Ok(home)) = (hook::words(&words), Home::resolve()) {
            hook::hook(&home, &words).await;
        }
        return;
    }
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("rl: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = Home::resolve()?;
    rl::trace::open(&home);
    rl::drift::install(&home);
    let Some(command) = cli.command else {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            anyhow::bail!("the list needs a terminal · rl ls lists sessions as text");
        }
        // The first start walks through setup once; never inside a held
        // session. `ctrl+c` there leaves without the list.
        if rl::setup::should_ask(&home, true, std::env::var_os("RL_SESSION").is_some()) {
            match rl::tui::wizard::first_run(&home) {
                Ok(rl::tui::wizard::model::Ended::Interrupted) => return Ok(()),
                Ok(_) => {}
                Err(e) => eprintln!("rl setup: {e:#}"),
            }
        }
        return rl::tui::run(&home).await;
    };
    match command {
        Command::New {
            harness,
            cwd,
            name_only,
            argv,
        } => cmd::new(&home, harness, cwd, name_only, argv).await,
        Command::Ls { json } => cmd::ls(&home, json),
        Command::Attach { name } => cmd::attach(&home, &name).await,
        Command::Stop { name } => cmd::stop(&home, &name).await,
        Command::Rm { name } => cmd::rm(&home, &name).await,
        Command::Color { session, colour } => {
            println!("{}", cmd::color(&home, &session, &colour)?);
            Ok(())
        }
        Command::Setup {
            check: true, harness, ..
        } => {
            if rl::setup::main_check(&home, &harness)? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Setup {
            remove, harness, yes, ..
        } => {
            use std::io::IsTerminal;
            let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            let in_session = std::env::var_os("RL_SESSION").is_some();
            if !remove && !yes && harness.is_empty() && terminal && !in_session {
                return rl::tui::wizard::command(&home);
            }
            rl::setup::main(&home, &harness, remove)
        }
        Command::Report { dismiss: true, .. } => {
            let n = rl::drift::dismiss(&rl::drift::dir(&home))?;
            println!("dismissed {n}");
            Ok(())
        }
        Command::Report { dry_run, .. } => {
            let stdin = std::io::stdin();
            rl::drift::report(
                &rl::drift::dir(&home),
                dry_run,
                &mut stdin.lock(),
                &mut std::io::stdout(),
            )
        }
        Command::Doctor => rl::drift::doctor(&home, &mut std::io::stdout()),
        // Blocks on stdin; the harnesses it asks block too.
        Command::Mcp { harness } => tokio::task::spawn_blocking(move || rl::mcp::run(&home, &harness)).await?,
    }
}

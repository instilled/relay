use anyhow::Result;
use clap::{Parser, Subcommand};
use rl::{cmd, hook};
use rl_core::Home;
use std::path::PathBuf;

/// Relay: list, attach and run shells for any agent. With no command, the
/// list.
#[derive(Parser)]
#[command(name = "rl", version)]
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
    /// Attach this terminal to a session (ctrl-\ detaches).
    Attach { name: String },
    /// Close a running session.
    Stop { name: String },
    /// Remove a session that is not running.
    Rm { name: String },
    /// Serve send, who and inbox to an agent over stdio (an MCP server).
    Mcp,
}

#[tokio::main]
async fn main() {
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
    let Some(command) = cli.command else {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            anyhow::bail!("the list needs a terminal · rl ls lists sessions as text");
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
        // Blocks on stdin; the adapters it asks block too.
        Command::Mcp => tokio::task::spawn_blocking(move || rl::mcp::run(&home)).await?,
    }
}

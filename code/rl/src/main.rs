use anyhow::Result;
use clap::{Parser, Subcommand};
use rl::cmd;
use rl_core::Home;
use std::path::PathBuf;

/// Relay: list, attach and run shells for any agent.
#[derive(Parser)]
#[command(name = "rl-new", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("rl: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = Home::resolve()?;
    match cli.command {
        Command::New {
            harness,
            cwd,
            name_only,
            argv,
        } => cmd::new(&home, harness, cwd, name_only, argv).await,
        Command::Ls { json } => cmd::ls(&home, json),
        Command::Attach { name } => cmd::attach(&home, &name).await,
        Command::Stop { name } => cmd::stop(&home, &name).await,
        Command::Rm { name } => cmd::rm(&home, &name),
    }
}

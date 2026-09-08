use anyhow::Result;
use clap::{Parser, Subcommand};
use relay_core::{Home, display_path};
use relay_proto::methods::DaemonInfo;
use rl_cli::daemon;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "rl",
    version,
    about = "Relay — keeps your agents and long-running work alive"
)]
struct Cli {
    /// Machine-readable output on every command.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the rld daemon.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = Home::resolve()?;
    match cli.cmd {
        Cmd::Daemon { cmd } => run_daemon(&home, cmd, cli.json).await,
    }
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

use clap::Parser;

/// Relay: list, attach and run shells for any agent.
#[derive(Parser)]
#[command(name = "rl-new", version)]
struct Cli {}

fn main() {
    Cli::parse();
}

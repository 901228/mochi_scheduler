mod cli;
mod gpu;
mod ipc;
mod settings;
mod utils;

use clap::Parser;
use cli::{Cli, Command};
use ipc::{client, daemon};
use settings::Settings;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli: Cli = Cli::parse();
    let settings = Settings::resolve()?;

    match cli.command {
        Command::Daemon => daemon::run(settings).await,
        other => client::run(settings, other).await,
    }
}

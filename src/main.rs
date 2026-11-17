mod cli;
mod config;
mod ipc;
mod placer;
mod reconciler;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List => {
            for name in config::list()? {
                println!("{name}");
            }
        }
        Command::Switch { profile } => switch(&profile, cli.timeout).await?,
        Command::Profile(args) if args.len() == 1 => switch(&args[0], cli.timeout).await?,
        Command::Profile(_) => bail!("expected a single profile name"),
        _ => bail!("this command requires the Niri backend"),
    }
    Ok(())
}

async fn switch(name: &str, timeout: u64) -> Result<()> {
    anyhow::ensure!(timeout > 0, "timeout must be positive");
    let profile = config::Profile::load(name)?;
    reconciler::switch(
        &ipc::Ipc::from_env()?,
        &profile,
        std::time::Duration::from_secs(timeout),
    )
    .await
}

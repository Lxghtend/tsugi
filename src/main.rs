mod cli;
mod config;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List => {
            for name in config::list()? { println!("{name}"); }
        }
        Command::Switch { profile } => validate(&profile)?,
        Command::Profile(args) if args.len() == 1 => validate(&args[0])?,
        Command::Profile(_) => bail!("expected a single profile name"),
        _ => bail!("this command requires the Niri backend"),
    }
    Ok(())
}

fn validate(name: &str) -> Result<()> {
    config::Profile::load(name)?;
    bail!("profile validated; switching requires the Niri backend")
}

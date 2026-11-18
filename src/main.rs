mod cli;
mod config;
mod hooks;
mod ipc;
mod placer;
mod reconciler;
mod snapshot;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use cli::{Cli, Command};
use ipc::Ipc;
use std::{fs, path::PathBuf, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(
            std::env::var("TSUGI_LOG")
                .ok()
                .and_then(|s| s.parse::<tracing::Level>().ok())
                .unwrap_or(tracing::Level::WARN),
        )
        .init();
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
        Command::Save {
            profile,
            ignore_focused_window,
            unmatched,
        } => {
            snapshot::save(
                &Ipc::from_env()?,
                &profile,
                ignore_focused_window,
                unmatched.unwrap_or(config::Policy::Ignore),
            )
            .await?
        }
        Command::Status => {
            let ipc = Ipc::from_env()?;
            println!(
                "{}",
                active(&state_dir(&ipc)?)?
                    .as_deref()
                    .unwrap_or("no active profile")
            );
        }
        Command::Daemon => {
            let mut events = Ipc::from_env()?.events().await?;
            loop {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => { result?; break; }
                    event = events.next() => { let event = event?; tracing::debug!(?event, "Niri event"); }
                }
            }
        }
    }
    Ok(())
}

fn state_dir(ipc: &Ipc) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // Include the compositor socket identity so parallel sessions stay separate.
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?);
    ensure!(runtime.is_absolute(), "XDG_RUNTIME_DIR must be absolute");
    let socket = ipc.0.as_os_str().as_bytes();
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    socket.hash(&mut hash);
    Ok(runtime
        .join("tsugi")
        .join(format!("{:016x}", hash.finish())))
}

fn active(dir: &std::path::Path) -> Result<Option<String>> {
    match fs::read_to_string(dir.join("active")) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn switch(name: &str, timeout: u64) -> Result<()> {
    ensure!(timeout > 0, "timeout must be positive");
    let limit = Duration::from_secs(timeout);
    let profile = config::Profile::load(name)?;
    let ipc = Ipc::from_env()?;
    let dir = state_dir(&ipc)?;
    fs::create_dir_all(&dir)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("switch.lock"))?;
    lock.try_lock()
        .context("could not acquire switch lock; another switch may be running")?;
    let settings = config::Settings::load()?;
    let original_workspaces = ipc.workspaces().await?;
    let old = active(&dir)?;
    let changed = old.as_deref() != Some(name);
    if changed {
        if let Some(old) = old {
            match config::Profile::load(&old) {
                Ok(profile) => hooks::run(&profile.on_exit, "on_exit", limit).await?,
                Err(err) => tracing::warn!(%err, "could not load previous profile for exit hooks"),
            }
        }
    }
    reconciler::switch(&ipc, &profile, &settings, limit).await?;
    if changed {
        hooks::run(&profile.on_enter, "on_enter", limit).await?;
    }
    reconciler::restore_workspace_focus(&ipc, &original_workspaces, &settings.focus_topmost)
        .await?;
    fs::write(dir.join("active.new"), name)?;
    fs::rename(dir.join("active.new"), dir.join("active"))?;
    Ok(())
}

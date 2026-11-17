use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio::{process::Command, time::timeout};

pub async fn run(commands: &[String], phase: &str, limit: Duration) -> Result<()> {
    for (index, script) in commands.iter().enumerate() {
        let status = timeout(
            limit,
            Command::new("/bin/sh")
                .args(["-c", script])
                .kill_on_drop(true)
                .status(),
        )
        .await
        .with_context(|| format!("{phase} hook {} timed out", index + 1))??;
        ensure!(
            status.success(),
            "{phase} hook {} failed: {status}",
            index + 1
        );
    }
    Ok(())
}

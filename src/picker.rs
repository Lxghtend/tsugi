use anyhow::{Context, Result, ensure};
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command};

pub async fn pick(mut profiles: Vec<String>, active: Option<&str>) -> Result<Option<String>> {
    profiles.retain(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    });
    ensure!(!profiles.is_empty(), "no profiles to pick from");
    profiles.sort_by_key(|name| Some(name.as_str()) != active);
    let mut rows = String::new();
    for name in &profiles {
        rows.push_str(name);
        if Some(name.as_str()) == active {
            rows.push_str(" (active)");
        }
        rows.push('\n');
    }
    let mut child = Command::new("fuzzel")
        .args([
            "--dmenu",
            "--index",
            "--only-match",
            "--prompt",
            "Profile: ",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting Fuzzel (install fuzzel and ensure it is on PATH)")?;
    let mut input = child.stdin.take().context("missing Fuzzel stdin")?;
    let write = input.write_all(rows.as_bytes()).await;
    drop(input);
    let result = child
        .wait_with_output()
        .await
        .context("waiting for Fuzzel")?;
    if result.status.code() == Some(1) {
        return Ok(None);
    }
    ensure!(
        result.status.success(),
        "Fuzzel exited with {}",
        result.status
    );
    write.context("sending profiles to Fuzzel")?;
    selection(&profiles, &result.stdout).map(Some)
}

fn selection(profiles: &[String], output: &[u8]) -> Result<String> {
    let index: usize = std::str::from_utf8(output)?
        .trim()
        .parse()
        .context("invalid Fuzzel selection index")?;
    profiles
        .get(index)
        .cloned()
        .context("Fuzzel selection index out of range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_resolves_only_listed_profiles() {
        let profiles = ["school".into(), "development".into()];
        assert_eq!(selection(&profiles, b"1\n").unwrap(), "development");
        assert!(selection(&profiles, b"2\n").is_err());
        assert!(selection(&profiles, b"school\n").is_err());
    }
}

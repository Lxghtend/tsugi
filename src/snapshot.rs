use crate::{
    config::{self, Column, Policy, Profile, Width, Window, Workspace},
    ipc::Ipc,
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, fs, io::Write, os::unix::fs::OpenOptionsExt};

pub async fn save(ipc: &Ipc, name: &str) -> Result<()> {
    let path = config::profile_path(name)?;
    let windows = ipc.windows().await?;
    let mut profile = Profile {
        name: name.into(),
        unmatched_policy: Policy::Ignore,
        stash_workspace: "_stash".into(),
        on_enter: vec![],
        on_exit: vec![],
        workspaces: vec![],
    };
    let mut workspaces = ipc.workspaces().await?;
    workspaces.sort_by_key(|w| (w.output.clone(), w.idx));
    for ws in workspaces {
        let Some(name) = ws.name else { continue };
        if name == "_stash" {
            continue;
        }
        let mut columns = BTreeMap::<usize, BTreeMap<usize, (i32, Window)>>::new();
        for w in windows.iter().filter(|w| w.workspace_id == Some(ws.id)) {
            let Some((column, tile)) = w.layout.pos_in_scrolling_layout else {
                continue;
            };
            let (Some(app), Some(pid)) = (&w.app_id, w.pid.filter(|p| *p > 0)) else {
                tracing::warn!(id = w.id, "skipping window without app ID or PID");
                continue;
            };
            let proc = format!("/proc/{pid}");
            let command = fs::read(format!("{proc}/cmdline"))
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            let Some(command) = command.filter(|s| !s.is_empty()) else {
                tracing::warn!(id = w.id, "skipping window with unreadable command line");
                continue;
            };
            let command = command
                .strip_suffix('\0')
                .unwrap_or(&command)
                .split('\0')
                .map(str::to_owned)
                .collect();
            let cwd = fs::read_link(format!("{proc}/cwd"))
                .ok()
                .and_then(|p| p.into_os_string().into_string().ok());
            columns.entry(column).or_default().insert(
                tile,
                (
                    w.layout.tile_size.0.round() as i32,
                    Window {
                        app_id: regex::escape(app),
                        title_match: None,
                        command,
                        cwd,
                    },
                ),
            );
        }
        let columns = columns
            .into_values()
            .map(|tiles| Column {
                width: Width::Pixels(
                    tiles
                        .values()
                        .map(|(width, _)| *width)
                        .max()
                        .unwrap_or(1)
                        .max(1),
                ),
                tabbed: None,
                windows: tiles.into_values().map(|(_, window)| window).collect(),
            })
            .collect();
        profile.workspaces.push(Workspace {
            name,
            output: ws.output,
            columns,
        });
    }
    ensure!(
        !profile.workspaces.is_empty(),
        "no named workspaces to snapshot"
    );
    profile.validate()?;
    let text = toml::to_string_pretty(&profile)?;
    fs::create_dir_all(config::directory()?)?;
    // Command lines can contain private arguments; don't make snapshots world-readable.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("creating {} (save does not overwrite)", path.display()))?
        .write_all(text.as_bytes())?;
    println!("{}", path.display());
    Ok(())
}

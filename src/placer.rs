use crate::{
    config::{Column, Matcher, Window, home},
    ipc::Ipc,
};
use anyhow::{Context, Result, ensure};
use niri_ipc::{Action, ColumnDisplay, Event, WorkspaceReferenceArg::Id};
use std::{collections::HashSet, os::unix::process::CommandExt, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

pub async fn spawn(ipc: &Ipc, spec: &Window, matcher: &Matcher, deadline: Duration) -> Result<u64> {
    let mut events = ipc.events().await?;
    // Consume the subscription baseline before launching, so initial windows
    // and later title changes on those windows cannot satisfy this ticket.
    let mut existing: HashSet<_> = timeout(deadline, async {
        loop {
            if let Event::WindowsChanged { windows } = events.next().await? {
                return Ok::<_, anyhow::Error>(windows.into_iter().map(|w| w.id).collect());
            }
        }
    })
    .await
    .context("waiting for initial Niri window list")??;
    existing.extend(ipc.windows().await?.into_iter().map(|w| w.id));
    let mut command = Command::new(&spec.command[0]);
    command
        .args(&spec.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = &spec.cwd {
        let path = if cwd == "~" {
            home()?
        } else if let Some(rest) = cwd.strip_prefix("~/") {
            home()?.join(rest)
        } else {
            cwd.into()
        };
        command.current_dir(path);
    }
    // setsid is async-signal-safe and detaches the app from the terminal.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("launching {}", spec.command[0]))?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    timeout(deadline, async {
        loop {
            if let Event::WindowOpenedOrChanged { window } = events.next().await? {
                if !existing.contains(&window.id) && matcher.matches(&window) {
                    return Ok(window.id);
                }
            }
        }
    })
    .await
    .with_context(|| format!("waiting for a new {} window", spec.app_id))?
}

pub async fn move_window(ipc: &Ipc, id: u64, workspace: u64) -> Result<()> {
    ipc.action(Action::MoveWindowToWorkspace {
        window_id: Some(id),
        reference: Id(workspace),
        focus: false,
    })
    .await
}

async fn focus(ipc: &Ipc, id: u64) -> Result<()> {
    ipc.action(Action::FocusWindow { id }).await?;
    ensure!(
        matches!(ipc.request(niri_ipc::Request::FocusedWindow).await?, niri_ipc::Response::FocusedWindow(Some(w)) if w.id == id),
        "could not focus window {id}"
    );
    Ok(())
}

pub async fn place(
    ipc: &Ipc,
    workspace: u64,
    index: usize,
    column: &Column,
    ids: &[u64],
) -> Result<()> {
    for (offset, &id) in ids.iter().enumerate() {
        move_window(ipc, id, workspace).await?;
        ipc.action(Action::MoveWindowToTiling { id: Some(id) })
            .await?;
        let windows = ipc.windows().await?;
        let window = windows
            .iter()
            .find(|w| w.id == id)
            .context("target window disappeared")?;
        let (col, _) = window
            .layout
            .pos_in_scrolling_layout
            .context("target window is not tiled")?;
        ensure!(
            window.workspace_id == Some(workspace),
            "window {id} did not move to its target workspace"
        );
        if windows
            .iter()
            .filter(|w| {
                w.workspace_id == Some(workspace)
                    && w.layout.pos_in_scrolling_layout.is_some_and(|p| p.0 == col)
            })
            .count()
            > 1
        {
            ipc.action(Action::ConsumeOrExpelWindowRight { id: Some(id) })
                .await?;
        }
        focus(ipc, id).await?;
        ipc.action(Action::MoveColumnToIndex {
            index: index + usize::from(offset != 0),
        })
        .await?;
        if offset != 0 {
            ipc.action(Action::ConsumeOrExpelWindowLeft { id: Some(id) })
                .await?;
        }
        let actual = ipc.windows().await?;
        ensure!(
            ids[..=offset]
                .iter()
                .enumerate()
                .all(|(tile, id)| actual.iter().any(|w| w.id == *id
                    && w.workspace_id == Some(workspace)
                    && w.layout.pos_in_scrolling_layout == Some((index, tile + 1)))),
            "could not arrange workspace {workspace}, column {index}"
        );
    }
    focus(ipc, ids[0]).await?;
    if let Some(tabbed) = column.tabbed {
        ipc.action(Action::SetColumnDisplay {
            display: if tabbed {
                ColumnDisplay::Tabbed
            } else {
                ColumnDisplay::Normal
            },
        })
        .await?;
    }
    ipc.action(Action::SetColumnWidth {
        change: column.width.change()?,
    })
    .await
}

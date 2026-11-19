use crate::{
    config::{Column, Matcher, Window, home},
    ipc::Ipc,
};
use anyhow::{Context, Result, ensure};
use niri_ipc::{Action, ColumnDisplay, Event, WorkspaceReferenceArg::Id};
use std::{collections::HashSet, os::unix::process::CommandExt, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

pub async fn spawn_missing(
    ipc: &Ipc,
    tickets: &[(&Window, Matcher)],
    deadline: Duration,
) -> Result<Vec<u64>> {
    if tickets.is_empty() {
        return Ok(Vec::new());
    }
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
    let pids = tickets
        .iter()
        .map(|(spec, _)| launch(spec))
        .collect::<Result<Vec<_>>>()?;
    let mut matched = vec![None; tickets.len()];
    timeout(deadline, async {
        while matched.contains(&None) {
            match events.next().await? {
                Event::WindowOpenedOrChanged { window } => {
                    if let Some(slot) = spawn_slot(tickets, &pids, &existing, &matched, &window) {
                        matched[slot] = Some(window.id);
                    }
                }
                Event::WindowClosed { id } => {
                    if let Some(slot) = matched.iter_mut().find(|slot| **slot == Some(id)) {
                        *slot = None;
                    }
                }
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| {
        let missing = tickets
            .iter()
            .zip(&matched)
            .filter_map(|((spec, _), id)| id.is_none().then_some(spec.app_id.as_str()))
            .collect::<Vec<_>>();
        format!("timed out waiting for new windows: {}", missing.join(", "))
    })??;
    Ok(matched
        .into_iter()
        .map(|id| id.expect("all tickets matched"))
        .collect())
}

fn spawn_slot(
    tickets: &[(&Window, Matcher)],
    pids: &[Option<i32>],
    existing: &HashSet<u64>,
    matched: &[Option<u64>],
    window: &niri_ipc::Window,
) -> Option<usize> {
    if existing.contains(&window.id) || matched.contains(&Some(window.id)) {
        return None;
    }
    tickets
        .iter()
        .enumerate()
        .filter(|(slot, (_, matcher))| matched[*slot].is_none() && matcher.matches(window))
        // Prefer the launching process when visible; wrappers and single-instance apps may report another PID.
        .min_by_key(|(slot, (spec, _))| {
            (
                window.pid.is_none() || window.pid != pids[*slot],
                spec.title_match.is_none(),
            )
        })
        .map(|(slot, _)| slot)
}

fn launch(spec: &Window) -> Result<Option<i32>> {
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
    let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(pid)
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

fn column_matches(windows: &[niri_ipc::Window], workspace: u64, index: usize, ids: &[u64]) -> bool {
    let positions = windows.iter().filter_map(|window| {
        let (column, tile) = window.layout.pos_in_scrolling_layout?;
        (window.workspace_id == Some(workspace) && !window.is_floating && column == index)
            .then_some((tile, window.id))
    });
    let mut count = 0;
    for (tile, id) in positions {
        if tile.checked_sub(1).and_then(|offset| ids.get(offset)) != Some(&id) {
            return false;
        }
        count += 1;
    }
    count == ids.len()
}

pub async fn place(
    ipc: &Ipc,
    workspace: u64,
    index: usize,
    column: &Column,
    ids: &[u64],
) -> Result<()> {
    let windows = ipc.windows().await?;
    let change = column.width.change()?;
    if column_matches(&windows, workspace, index, ids) {
        // IPC exposes rendered tile sizes, not the configured column width or tab mode.
        if column.tabbed.is_none()
            && matches!(change, niri_ipc::SizeChange::SetFixed(px) if ids.iter().all(|id| {
                windows.iter().any(|w| w.id == *id && (w.layout.tile_size.0 - f64::from(px)).abs() <= 0.5)
            }))
        {
            return Ok(());
        }
    } else {
        arrange(ipc, workspace, index, ids).await?;
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
    ipc.action(Action::SetColumnWidth { change }).await
}

async fn arrange(ipc: &Ipc, workspace: u64, index: usize, ids: &[u64]) -> Result<()> {
    for (offset, &id) in ids.iter().enumerate() {
        let windows = ipc.windows().await?;
        let window = windows
            .iter()
            .find(|w| w.id == id)
            .context("target window disappeared")?;
        if window.workspace_id != Some(workspace) {
            move_window(ipc, id, workspace).await?;
        }
        if window.is_floating {
            ipc.action(Action::MoveWindowToTiling { id: Some(id) })
                .await?;
        }
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
        let destination = index + usize::from(offset != 0);
        let current = ipc
            .windows()
            .await?
            .into_iter()
            .find(|w| w.id == id)
            .and_then(|w| w.layout.pos_in_scrolling_layout)
            .context("target window is not tiled")?
            .0;
        if current != destination {
            focus(ipc, id).await?;
            ipc.action(Action::MoveColumnToIndex { index: destination })
                .await?;
        }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use niri_ipc::{Window, WindowLayout};

    fn window(id: u64, workspace: u64, column: usize, tile: usize) -> Window {
        Window {
            id,
            title: None,
            app_id: None,
            pid: None,
            workspace_id: Some(workspace),
            is_focused: false,
            is_floating: false,
            is_urgent: false,
            focus_timestamp: None,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((column, tile)),
                tile_size: (0.0, 0.0),
                window_size: (0, 0),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0.0, 0.0),
            },
        }
    }

    #[test]
    fn concurrent_tickets_do_not_claim_the_same_window_twice() {
        let spec = crate::config::Window {
            app_id: "kitty".into(),
            title_match: None,
            command: vec!["kitty".into()],
            cwd: None,
        };
        let tickets = [
            (&spec, spec.matcher().unwrap()),
            (&spec, spec.matcher().unwrap()),
        ];
        let pids = [Some(10), Some(20)];
        let mut w = window(2, 1, 1, 1);
        w.app_id = Some("kitty".into());
        w.pid = Some(20);
        let existing = HashSet::from([1]);
        assert_eq!(
            spawn_slot(&tickets, &pids, &existing, &[None, None], &w),
            Some(1)
        );
        assert_eq!(
            spawn_slot(&tickets, &pids, &existing, &[None, Some(2)], &w),
            None
        );
        w.id = 1;
        assert_eq!(
            spawn_slot(&tickets, &pids, &existing, &[None, Some(2)], &w),
            None
        );
        w.id = 3;
        w.pid = None;
        assert_eq!(
            spawn_slot(&tickets, &pids, &existing, &[None, Some(2)], &w),
            Some(0)
        );
    }

    #[test]
    fn only_exact_column_layouts_skip_arrangement() {
        let mut windows = vec![
            window(2, 10, 1, 2),
            window(1, 10, 1, 1),
            window(3, 20, 1, 1),
        ];
        assert!(column_matches(&windows, 10, 1, &[1, 2]));
        assert!(!column_matches(&windows, 10, 1, &[2, 1]));
        assert!(!column_matches(&windows, 10, 1, &[1]));
        assert!(!column_matches(&windows, 10, 2, &[1, 2]));
        windows[0].is_floating = true;
        assert!(!column_matches(&windows, 10, 1, &[1, 2]));
    }
}

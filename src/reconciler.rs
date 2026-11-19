use crate::{
    config::{FocusTopmost, Policy, Profile, Settings},
    ipc::Ipc,
    placer,
};
use anyhow::{Context, Result, ensure};
use niri_ipc::{Action, Request, Response, WorkspaceReferenceArg::Id};
use std::{collections::BTreeMap, time::Duration};

pub async fn switch(
    ipc: &Ipc,
    profile: &Profile,
    settings: &Settings,
    deadline: Duration,
) -> Result<()> {
    let workspaces = ipc.workspaces().await?;
    let lookup = |name: &str| {
        workspaces
            .iter()
            .find(|w| w.name.as_deref() == Some(name))
            .with_context(|| format!("define workspace {name:?} in your Niri configuration first"))
    };
    let targets = profile
        .workspaces
        .iter()
        .map(|ws| lookup(&ws.name))
        .collect::<Result<Vec<_>>>()?;
    let stash = if profile.unmatched_policy == Policy::Stash
        || (profile.unmatched_policy == Policy::Close && !settings.always_stash.is_empty())
    {
        let id = lookup(&profile.stash_workspace)?.id;
        ensure!(
            !targets.iter().any(|ws| ws.id == id),
            "stash workspace cannot also be a target"
        );
        Some(id)
    } else {
        None
    };
    if profile.workspaces.iter().any(|w| w.output.is_some()) {
        let Response::Outputs(outputs) = ipc.request(Request::Outputs).await? else {
            anyhow::bail!("expected outputs reply")
        };
        for ws in &profile.workspaces {
            if let Some(output) = &ws.output {
                ensure!(
                    outputs.get(output).is_some_and(|o| o.logical.is_some()),
                    "output {output} is unavailable"
                );
            }
        }
    }
    // Placement needs normal window focus; overview can intercept it.
    match ipc.request(Request::OverviewState).await? {
        Response::OverviewState(state) if state.is_open => {
            ipc.action(Action::CloseOverview {}).await?;
            ensure!(
                matches!(ipc.request(Request::OverviewState).await?, Response::OverviewState(state) if !state.is_open),
                "Niri overview did not close; cannot arrange windows"
            );
        }
        Response::OverviewState(_) => {}
        _ => anyhow::bail!("expected Niri overview state"),
    }
    let mut leftovers = ipc.windows().await?;
    leftovers.sort_by_key(|w| w.id);
    // Reserve exact matches first so a fallback cannot steal a later slot.
    let mut exact = BTreeMap::new();
    for (ws, target) in profile.workspaces.iter().zip(&targets) {
        for (col, column) in ws.columns.iter().enumerate() {
            for (tile, spec) in column.windows.iter().enumerate() {
                let position = (col + 1, tile + 1);
                let matcher = spec.matcher()?;
                if let Some(index) = leftovers.iter().position(|w| {
                    w.workspace_id == Some(target.id)
                        && !w.is_floating
                        && w.layout.pos_in_scrolling_layout == Some(position)
                        && matcher.matches(w)
                }) {
                    exact.insert((target.id, position), leftovers.remove(index).id);
                }
            }
        }
    }
    let mut plan = Vec::new();
    for (ws, target) in profile.workspaces.iter().zip(&targets) {
        for (index, column) in ws.columns.iter().enumerate() {
            let mut ids = Vec::new();
            for (tile, spec) in column.windows.iter().enumerate() {
                if let Some(id) = exact.remove(&(target.id, (index + 1, tile + 1))) {
                    ids.push(id);
                    continue;
                }
                let matcher = spec.matcher()?;
                let found = leftovers
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| matcher.matches(w))
                    .min_by_key(|(_, w)| w.workspace_id != Some(target.id))
                    .map(|(index, _)| index);
                ids.push(match found {
                    Some(index) => leftovers.remove(index).id,
                    None => placer::spawn(ipc, spec, &matcher, deadline).await?,
                });
            }
            plan.push((target.id, index + 1, column, ids));
        }
    }
    for (ws, target) in profile.workspaces.iter().zip(&targets) {
        if let Some(output) = &ws.output {
            if target.output.as_ref() != Some(output) {
                ipc.action(Action::MoveWorkspaceToMonitor {
                    output: output.clone(),
                    reference: Some(Id(target.id)),
                })
                .await?;
            }
        }
    }
    for (workspace, index, column, ids) in plan {
        placer::place(ipc, workspace, index, column, &ids).await?;
    }
    for window in leftovers {
        match settings.unmatched_policy(profile.unmatched_policy, window.app_id.as_deref()) {
            Policy::Ignore => {}
            Policy::Close => {
                ipc.action(Action::CloseWindow {
                    id: Some(window.id),
                })
                .await?
            }
            Policy::Stash => {
                let target = stash.expect("stash workspace was resolved");
                if window.workspace_id != Some(target) {
                    placer::move_window(ipc, window.id, target).await?;
                }
            }
        }
    }
    Ok(())
}

pub async fn restore_workspace_focus(
    ipc: &Ipc,
    original: &[niri_ipc::Workspace],
    settings: &FocusTopmost,
) -> Result<()> {
    let workspaces = ipc.workspaces().await?;
    for id in workspace_focus_order(original, &workspaces, settings) {
        ipc.action(Action::FocusWorkspace { reference: Id(id) })
            .await?;
    }
    Ok(())
}

fn workspace_focus_order(
    original: &[niri_ipc::Workspace],
    workspaces: &[niri_ipc::Workspace],
    settings: &FocusTopmost,
) -> Vec<u64> {
    let original_output = original
        .iter()
        .find(|ws| ws.is_focused)
        .and_then(|ws| ws.output.as_deref());
    let mut topmost = BTreeMap::<&str, &niri_ipc::Workspace>::new();
    for ws in workspaces {
        let Some(output) = ws.output.as_deref() else {
            continue;
        };
        let first = topmost.entry(output).or_insert(ws);
        if ws.idx < first.idx {
            *first = ws;
        }
    }
    let restore = original_output
        .filter(|output| topmost.contains_key(output))
        .or_else(|| {
            workspaces
                .iter()
                .find(|ws| ws.is_focused)
                .and_then(|ws| ws.output.as_deref())
        });
    // Only visit other outputs that need a workspace change. Visiting one
    // can steal keyboard focus, so restore the original output afterward.
    let destination = |output: &str, first| {
        if settings.enabled(output) {
            return Some(first);
        }
        let previous = original
            .iter()
            .find(|ws| ws.output.as_deref() == Some(output) && ws.is_active);
        // A dynamic workspace might disappear or move to another output during placement.
        previous
            .and_then(|prev| {
                workspaces
                    .iter()
                    .find(|ws| ws.id == prev.id && ws.output.as_deref() == Some(output))
            })
            .or_else(|| {
                workspaces
                    .iter()
                    .find(|ws| ws.output.as_deref() == Some(output) && ws.is_active)
            })
    };
    let last = restore.and_then(|output| {
        topmost
            .remove(output)
            .and_then(|first| destination(output, first))
    });
    let mut changes = topmost
        .into_iter()
        .filter_map(|(output, first)| destination(output, first))
        .filter(|ws| !ws.is_active)
        .map(|ws| ws.id)
        .collect::<Vec<_>>();
    if let Some(ws) = last.filter(|ws| !changes.is_empty() || !ws.is_focused || !ws.is_active) {
        changes.push(ws.id);
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use niri_ipc::Workspace;

    fn workspace(id: u64, idx: u8, output: &str, active: bool, focused: bool) -> Workspace {
        Workspace {
            id,
            idx,
            name: None,
            output: Some(output.into()),
            is_urgent: false,
            is_active: active,
            is_focused: focused,
            active_window_id: None,
        }
    }

    #[test]
    fn restores_previous_workspace_on_output_with_topmost_disabled() {
        let settings = FocusTopmost {
            outputs: [("HDMI-A-1".into(), false)].into(),
            ..FocusTopmost::default()
        };
        let original = [
            workspace(12, 2, "HDMI-A-1", true, false),
            workspace(21, 1, "DP-1", true, true),
        ];
        let after = [
            workspace(11, 1, "HDMI-A-1", false, false),
            workspace(12, 2, "HDMI-A-1", false, false),
            workspace(13, 3, "HDMI-A-1", true, true),
            workspace(21, 1, "DP-1", true, false),
        ];
        assert_eq!(
            workspace_focus_order(&original, &after, &settings),
            [12, 21]
        );
    }
}

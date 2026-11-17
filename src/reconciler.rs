use crate::{
    config::{Policy, Profile},
    ipc::Ipc,
    placer,
};
use anyhow::{Context, Result, ensure};
use niri_ipc::{Action, Request, Response, WorkspaceReferenceArg::Id};
use std::time::Duration;

pub async fn switch(ipc: &Ipc, profile: &Profile, deadline: Duration) -> Result<()> {
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
    let stash = if profile.unmatched_policy == Policy::Stash {
        Some(lookup(&profile.stash_workspace)?.id)
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
    let mut leftovers = ipc.windows().await?;
    leftovers.sort_by_key(|w| w.id);
    let mut plan = Vec::new();
    for (ws, target) in profile.workspaces.iter().zip(&targets) {
        for (index, column) in ws.columns.iter().enumerate() {
            let mut ids = Vec::new();
            for spec in &column.windows {
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
        match profile.unmatched_policy {
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
    if let Some(first) = targets.first() {
        ipc.action(Action::FocusWorkspace {
            reference: Id(first.id),
        })
        .await?;
    }
    Ok(())
}

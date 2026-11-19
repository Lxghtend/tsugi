use crate::{
    config::{self, Column, Policy, Profile, Width, Window, Workspace},
    ipc::Ipc,
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, env, fs, io::Write, os::unix::fs::PermissionsExt, path::Path};

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn on_path(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|dir| dir.is_absolute() && executable(&dir.join(program)))
    })
}

fn launch_command(
    app: &str,
    cmdline: Option<&str>,
    cwd: Option<String>,
    on_path: impl Fn(&str) -> bool,
) -> Option<(Vec<String>, Option<String>)> {
    // Vesktop's Electron process can rewrite argv and bypasses the packaged wrapper.
    if app == "vesktop" && on_path("vesktop") {
        return Some((vec!["vesktop".into()], None));
    }

    let raw = cmdline?;
    let args: Vec<_> = raw.strip_suffix('\0').unwrap_or(raw).split('\0').collect();
    let program = *args.first()?;
    if program.is_empty() {
        return None;
    }
    let path = Path::new(program);
    let usable = if path.is_absolute() {
        executable(path)
    } else if program.contains('/') {
        cwd.as_ref()
            .is_some_and(|dir| executable(&Path::new(dir).join(path)))
    } else {
        on_path(program)
    };
    usable.then(|| (args.into_iter().map(str::to_owned).collect(), cwd))
}

pub async fn save(
    ipc: &Ipc,
    name: &str,
    ignore_focused_window: bool,
    unmatched_policy: Policy,
    overwrite: bool,
) -> Result<()> {
    let path = config::profile_path(name)?;
    let mut windows = ipc.windows().await?;
    if ignore_focused_window {
        windows.retain(|window| !window.is_focused);
    }
    let mut profile = Profile {
        name: name.into(),
        unmatched_policy,
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
            let cwd = fs::read_link(format!("{proc}/cwd"))
                .ok()
                .filter(|path| path.is_dir())
                .and_then(|p| p.into_os_string().into_string().ok());
            let Some((command, cwd)) = launch_command(app, command.as_deref(), cwd, on_path) else {
                tracing::warn!(id = w.id, app_id = %app, "omitting window: no usable launch command; add it manually with the app's launcher");
                continue;
            };
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
    write_snapshot(&path, &text, overwrite)?;
    println!("{}", path.display());
    Ok(())
}

fn write_snapshot(path: &Path, text: &str, overwrite: bool) -> Result<()> {
    let parent = path.parent().context("snapshot path has no parent")?;
    fs::create_dir_all(parent)?;
    // Use a private temporary file on the destination filesystem for atomic publication.
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(text.as_bytes())?;
    if overwrite {
        match fs::symlink_metadata(path) {
            Ok(meta) => ensure!(
                meta.is_file(),
                "refusing to overwrite a symlink or non-regular file: {}",
                path.display()
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("checking {}", path.display())),
        }
        temp.persist(path)
            .with_context(|| format!("replacing {}", path.display()))?;
    } else {
        temp.persist_noclobber(path).with_context(|| {
            format!(
                "creating {} (use --overwrite to replace an existing profile)",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{fs, launch_command, write_snapshot};

    #[test]
    fn overwrite_requires_opt_in_and_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        write_snapshot(&path, "first", false).unwrap();
        assert!(write_snapshot(&path, "second", false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        write_snapshot(&path, "second", true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");

        let link = dir.path().join("managed.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(write_snapshot(&link, "third", true).is_err());
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn rewritten_electron_argv_uses_vesktop_launcher() {
        let raw = "/nix/store/example-electron/bin/electron --relaunch /nix/store/example-vesktop/app.asar\0";
        assert_eq!(
            launch_command(
                "vesktop",
                Some(raw),
                Some("/nix/store/example".into()),
                |p| p == "vesktop"
            ),
            Some((vec!["vesktop".into()], None)),
        );
        assert!(launch_command("vesktop", Some(raw), None, |_| false).is_none());
    }

    #[test]
    fn preserves_spaces_and_empty_arguments() {
        assert_eq!(
            launch_command("editor", Some("editor\0file with spaces\0\0"), None, |p| p
                == "editor"),
            Some((
                vec!["editor".into(), "file with spaces".into(), String::new()],
                None
            )),
        );
    }
}

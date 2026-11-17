use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, env, fs, path::PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub unmatched_policy: Policy,
    #[serde(default = "stash")]
    pub stash_workspace: String,
    #[serde(default)]
    pub on_enter: Vec<String>,
    #[serde(default)]
    pub on_exit: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}

fn stash() -> String {
    "_stash".into()
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Stash,
    Close,
    #[default]
    Ignore,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub name: String,
    pub output: Option<String>,
    #[serde(default)]
    pub columns: Vec<Column>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub width: Width,
    pub tabbed: Option<bool>,
    pub windows: Vec<Window>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Width {
    Pixels(i32),
    Text(String),
}

impl Width {
    pub fn change(&self) -> Result<niri_ipc::SizeChange> {
        use niri_ipc::SizeChange::*;
        let change = match self {
            Self::Pixels(n) => SetFixed(*n),
            Self::Text(s) => s
                .strip_suffix("px")
                .unwrap_or(s)
                .trim()
                .parse()
                .map_err(anyhow::Error::msg)?,
        };
        ensure!(
            matches!(change, SetFixed(n) if n > 0)
                || matches!(change, SetProportion(n) if n.is_finite() && n > 0.0),
            "width must be positive and absolute"
        );
        Ok(change)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub app_id: String,
    pub title_match: Option<String>,
    pub command: Vec<String>,
    pub cwd: Option<String>,
}

impl Window {
    pub fn matcher(&self) -> Result<Matcher> {
        ensure!(!self.app_id.is_empty(), "empty app_id");
        Ok(Matcher {
            app: Regex::new(&format!(r"\A(?:{})\z", self.app_id))
                .context("invalid app_id regex")?,
            title: self
                .title_match
                .as_deref()
                .map(Regex::new)
                .transpose()
                .context("invalid title_match regex")?,
        })
    }
}

pub struct Matcher {
    app: Regex,
    title: Option<Regex>,
}
impl Matcher {
    pub fn matches(&self, window: &niri_ipc::Window) -> bool {
        window
            .app_id
            .as_deref()
            .is_some_and(|id| self.app.is_match(id))
            && self.title.as_ref().is_none_or(|re| {
                window
                    .title
                    .as_deref()
                    .is_some_and(|title| re.is_match(title))
            })
    }
}

pub fn home() -> Result<PathBuf> {
    Ok(env::var_os("HOME").context("HOME is unset")?.into())
}

pub fn directory() -> Result<PathBuf> {
    Ok(env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(home()?.join(".config"))
        .join("tsugi/profiles"))
}

pub fn profile_path(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "invalid profile name: {name}"
    );
    Ok(directory()?.join(format!("{name}.toml")))
}

impl Profile {
    pub fn load(name: &str) -> Result<Self> {
        let path = profile_path(name)?;
        let profile: Self = toml::from_str(
            &fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
        )
        .with_context(|| format!("parsing {}", path.display()))?;
        ensure!(profile.name == name, "profile name must match filename");
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<()> {
        let mut names = HashSet::new();
        ensure!(
            !self.stash_workspace.trim().is_empty(),
            "empty stash workspace name"
        );
        for ws in &self.workspaces {
            ensure!(
                !ws.name.trim().is_empty() && names.insert(&ws.name),
                "empty or duplicate workspace: {:?}",
                ws.name
            );
            ensure!(
                self.unmatched_policy != Policy::Stash || ws.name != self.stash_workspace,
                "stash workspace cannot also be a target"
            );
            for col in &ws.columns {
                col.width.change()?;
                ensure!(!col.windows.is_empty(), "empty column on {}", ws.name);
                for window in &col.windows {
                    window.matcher()?;
                    ensure!(
                        window.command.first().is_some_and(|s| !s.is_empty()),
                        "missing command for {}",
                        window.app_id
                    );
                }
            }
        }
        Ok(())
    }
}

pub fn list() -> Result<Vec<String>> {
    let entries = match fs::read_dir(directory()?) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(name.to_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_round_trip() {
        for input in [
            include_str!("../profiles/default.toml"),
            include_str!("../profiles/developing.toml"),
            include_str!("../profiles/gaming.toml"),
        ] {
            let profile: Profile = toml::from_str(input).unwrap();
            profile.validate().unwrap();
            toml::from_str::<Profile>(&toml::to_string_pretty(&profile).unwrap())
                .unwrap()
                .validate()
                .unwrap();
        }
    }
}

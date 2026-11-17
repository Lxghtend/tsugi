use anyhow::{Context, Result, bail};
use niri_ipc::{Action, Event, Reply, Request, Response, Window, Workspace};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::UnixStream,
    time::timeout,
};

pub struct Ipc(pub PathBuf);
pub struct Events(Lines<BufReader<UnixStream>>);

impl Events {
    pub async fn next(&mut self) -> Result<Event> {
        let line = self
            .0
            .next_line()
            .await?
            .context("Niri event stream disconnected")?;
        Ok(serde_json::from_str(&line).context("decoding Niri event")?)
    }
}

impl Ipc {
    pub fn from_env() -> Result<Self> {
        Ok(Self(
            std::env::var_os("NIRI_SOCKET")
                .context("NIRI_SOCKET is not set")?
                .into(),
        ))
    }

    async fn open(&self, request: Request) -> Result<(Response, Events)> {
        timeout(Duration::from_secs(5), async {
            let mut stream = UnixStream::connect(&self.0)
                .await
                .with_context(|| format!("connecting to {}", self.0.display()))?;
            let mut message = serde_json::to_vec(&request)?;
            message.push(b'\n');
            stream.write_all(&message).await?;
            let mut lines = BufReader::new(stream).lines();
            let line = lines
                .next_line()
                .await?
                .context("Niri closed the connection")?;
            let reply: Reply = serde_json::from_str(&line).context("decoding Niri reply")?;
            Ok((reply.map_err(anyhow::Error::msg)?, Events(lines)))
        })
        .await
        .context("Niri IPC request timed out")?
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        Ok(self.open(request).await?.0)
    }
    pub async fn windows(&self) -> Result<Vec<Window>> {
        match self.request(Request::Windows).await? {
            Response::Windows(w) => Ok(w),
            _ => bail!("expected Niri windows reply"),
        }
    }
    pub async fn workspaces(&self) -> Result<Vec<Workspace>> {
        match self.request(Request::Workspaces).await? {
            Response::Workspaces(w) => Ok(w),
            _ => bail!("expected Niri workspaces reply"),
        }
    }
    pub async fn action(&self, action: Action) -> Result<()> {
        tracing::debug!(?action, "Niri action");
        match self.request(Request::Action(action)).await? {
            Response::Handled => Ok(()),
            _ => bail!("expected Niri action acknowledgment"),
        }
    }
    pub async fn events(&self) -> Result<Events> {
        let (reply, events) = self.open(Request::EventStream).await?;
        if !matches!(reply, Response::Handled) {
            bail!("expected Niri event stream acknowledgment");
        }
        Ok(events)
    }
}

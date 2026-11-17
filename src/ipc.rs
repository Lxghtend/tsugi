use anyhow::{Context, Result, bail};
use niri_ipc::{Action, Event, Reply, Request, Response, Window, Workspace};
use std::{cell::RefCell, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::UnixStream,
    time::timeout,
};

type Connection = Lines<BufReader<UnixStream>>;

pub struct Ipc(pub PathBuf, RefCell<Option<Connection>>);
pub struct Events(Connection);

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
            RefCell::new(None),
        ))
    }

    async fn exchange(
        &self,
        connection: Option<Connection>,
        request: Request,
    ) -> Result<(Reply, Connection)> {
        timeout(Duration::from_secs(5), async {
            let mut lines = match connection {
                Some(lines) => lines,
                None => BufReader::new(
                    UnixStream::connect(&self.0)
                        .await
                        .with_context(|| format!("connecting to {}", self.0.display()))?,
                )
                .lines(),
            };
            let mut message = serde_json::to_vec(&request)?;
            message.push(b'\n');
            lines.get_mut().get_mut().write_all(&message).await?;
            let line = lines
                .next_line()
                .await?
                .context("Niri closed the connection")?;
            let reply = serde_json::from_str(&line).context("decoding Niri reply")?;
            Ok((reply, lines))
        })
        .await
        .context("Niri IPC request timed out")?
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        // Own the connection during the exchange so a timeout or cancellation
        // drops it rather than leaving an unread reply for the next request.
        let connection = self.1.take();
        let (reply, connection) = self.exchange(connection, request).await?;
        self.1.replace(Some(connection));
        reply.map_err(anyhow::Error::msg)
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
        let (reply, connection) = self.exchange(None, Request::EventStream).await?;
        if !matches!(reply.map_err(anyhow::Error::msg)?, Response::Handled) {
            bail!("expected Niri event stream acknowledgment");
        }
        Ok(Events(connection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reuses_request_connection() {
        let (client, server) = UnixStream::pair().unwrap();
        let ipc = Ipc(
            PathBuf::new(),
            RefCell::new(Some(BufReader::new(client).lines())),
        );
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(server).lines();
            for request in [Request::Version, Request::Windows] {
                assert_eq!(
                    lines.next_line().await.unwrap().unwrap(),
                    serde_json::to_string(&request).unwrap()
                );
                lines
                    .get_mut()
                    .get_mut()
                    .write_all(b"{\"Ok\":\"Handled\"}\n")
                    .await
                    .unwrap();
            }
        });
        assert!(matches!(
            ipc.request(Request::Version).await.unwrap(),
            Response::Handled
        ));
        assert!(matches!(
            ipc.request(Request::Windows).await.unwrap(),
            Response::Handled
        ));
        server.await.unwrap();
    }
}

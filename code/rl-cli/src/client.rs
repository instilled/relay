//! Typed JSON-RPC client over the daemon socket.

use anyhow::{Context, Result, anyhow, bail};
use relay_core::Home;
use relay_proto::codec::{read_message, write_message};
use relay_proto::methods::Method;
use relay_proto::rpc::{Id, Request, Response};
use std::io::ErrorKind;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub struct Client {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    next_id: u64,
}

/// Distinguishes "daemon not running" from every other failure so callers can
/// offer `rl daemon start`.
#[derive(Debug)]
pub struct NotRunning;

impl std::fmt::Display for NotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rld is not running (start it with `rl daemon start`)")
    }
}
impl std::error::Error for NotRunning {}

impl Client {
    pub async fn connect(home: &Home) -> Result<Self> {
        let path = home.daemon_socket();
        let stream = match UnixStream::connect(&path).await {
            Ok(s) => s,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                return Err(anyhow!(NotRunning));
            }
            Err(e) => return Err(e).with_context(|| format!("connecting to {}", path.display())),
        };
        let (rd, wr) = stream.into_split();
        Ok(Self {
            rd: BufReader::new(rd),
            wr,
            next_id: 1,
        })
    }

    pub async fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Result> {
        let id = Id::Num(self.next_id);
        self.next_id += 1;
        let params = match serde_json::to_value(&params)? {
            serde_json::Value::Null => None,
            v => Some(v),
        };
        write_message(&mut self.wr, &Request::new(id.clone(), M::NAME, params)).await?;
        let res: Response = read_message(&mut self.rd)
            .await?
            .ok_or_else(|| anyhow!("daemon closed the connection during {}", M::NAME))?;
        if res.id != id {
            bail!("response id mismatch on {}", M::NAME);
        }
        if let Some(e) = res.error {
            return Err(anyhow!(e).context(M::NAME));
        }
        let v = res.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(v).with_context(|| format!("decoding {} result", M::NAME))
    }
}

//! Framing: one JSON value per line, `\n`-terminated, UTF-8.
//!
//! Lines rather than length prefixes so `nc -U ~/.relay/run/rld.sock` is a
//! usable debugging client.

use serde::{Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub async fn write_message<W, T>(w: &mut W, msg: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut buf = serde_json::to_vec(msg).map_err(io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf).await?;
    w.flush().await
}

/// `Ok(None)` on clean EOF. A malformed line is an `InvalidData` error.
pub async fn read_message<R, T>(r: &mut R) -> io::Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return serde_json::from_str(trimmed)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{Id, Request};
    use tokio::io::BufReader;

    #[tokio::test]
    async fn roundtrip_over_duplex() {
        let (mut a, b) = tokio::io::duplex(1024);
        let mut b = BufReader::new(b);
        write_message(&mut a, &Request::new(Id::Num(7), "daemon.ping", None))
            .await
            .unwrap();
        drop(a);
        let got: Request = read_message(&mut b).await.unwrap().unwrap();
        assert_eq!(got.method, "daemon.ping");
        assert_eq!(got.id, Id::Num(7));
        let eof: Option<Request> = read_message(&mut b).await.unwrap();
        assert!(eof.is_none());
    }
}

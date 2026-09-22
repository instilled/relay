//! Client side of the holder protocol — what `rld` (and tests) use.

use crate::proto::{Frame, FrameReader, Hello, write_frame};
use anyhow::{Context, Result, bail};
use std::io;
use std::path::Path;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub struct Session {
    rd: SessionReader,
    wr: SessionWriter,
}

/// Cancel-safe: `rld` reads holder frames inside a `select!` on both the
/// viewer path and the follow loop, and a half-read frame must survive
/// losing that race — see [`FrameReader`].
pub struct SessionReader(FrameReader<BufReader<OwnedReadHalf>>);
pub struct SessionWriter(OwnedWriteHalf);

impl Session {
    /// Connects and attaches. With `replay`, the first `Output` frames replay
    /// the ring buffer up to `ReplayEnd`.
    pub async fn attach(socket: &Path, replay: bool) -> Result<(Hello, Self)> {
        Self::attach_from(socket, if replay { Some(0) } else { None }).await
    }

    /// Attaches and replays from byte offset `from` of the child's output
    /// (`None`: no replay). `hello.replay_from` says where the replay really
    /// starts if the ring no longer reaches back that far.
    pub async fn attach_from(socket: &Path, from: Option<u64>) -> Result<(Hello, Self)> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let (rd, wr) = stream.into_split();
        let mut s = Self {
            rd: SessionReader(FrameReader::new(BufReader::new(rd))),
            wr: SessionWriter(wr),
        };
        s.wr.send(&Frame::Attach {
            replay: from.is_some(),
            from: from.unwrap_or(0),
        })
        .await?;
        match s.rd.recv().await? {
            Some(Frame::Hello(h)) => Ok((h, s)),
            other => bail!("expected hello, got {other:?}"),
        }
    }

    pub async fn recv(&mut self) -> io::Result<Option<Frame>> {
        self.rd.recv().await
    }

    pub async fn send(&mut self, f: &Frame) -> io::Result<()> {
        self.wr.send(f).await
    }

    pub async fn input(&mut self, bytes: impl Into<Vec<u8>>) -> io::Result<()> {
        self.wr.input(bytes).await
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.wr.resize(cols, rows).await
    }

    pub async fn signal(&mut self, n: i32) -> io::Result<()> {
        self.wr.signal(n).await
    }

    pub async fn close(&mut self) -> io::Result<()> {
        self.wr.close().await
    }

    /// Independent halves for concurrent read and write.
    pub fn split(self) -> (SessionReader, SessionWriter) {
        (self.rd, self.wr)
    }
}

impl SessionReader {
    pub async fn recv(&mut self) -> io::Result<Option<Frame>> {
        self.0.next().await
    }
}

impl SessionWriter {
    pub async fn send(&mut self, f: &Frame) -> io::Result<()> {
        write_frame(&mut self.0, f).await
    }
    pub async fn input(&mut self, bytes: impl Into<Vec<u8>>) -> io::Result<()> {
        self.send(&Frame::Input(bytes.into())).await
    }
    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.send(&Frame::Resize { cols, rows }).await
    }
    pub async fn signal(&mut self, n: i32) -> io::Result<()> {
        self.send(&Frame::Signal(n)).await
    }
    pub async fn close(&mut self) -> io::Result<()> {
        self.send(&Frame::Close).await
    }
}

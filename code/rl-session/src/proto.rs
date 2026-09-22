//! Holder wire protocol. Deliberately not JSON-RPC and not shared with
//! `rl-proto`: a session must stay reachable with nothing but this binary
//! alive, so the protocol is frozen and tiny.
//!
//! Frame = `tag: u8`, `len: u32` (big-endian), `payload`. Text payloads
//! (`Hello`, `Exit`) are JSON; everything else is raw bytes or fixed-width.
//!
//! Read frames through [`FrameReader`], never [`read_frame`], anywhere the
//! read shares a `select!` with anything else. `read_frame` holds a
//! half-read frame on the stack of the future doing the reading, so losing
//! that race drops those bytes for good and every read after it lands in the
//! middle of a payload. [`FrameReader`] keeps the same state in the struct,
//! so a poll that loses carries on where it left off.

use crate::exit::Exit;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest payload accepted on either side. Output is chunked well below this.
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

const T_ATTACH: u8 = 0x01;
const T_INPUT: u8 = 0x02;
const T_RESIZE: u8 = 0x03;
const T_SIGNAL: u8 = 0x04;
const T_CLOSE: u8 = 0x05;
const T_HELLO: u8 = 0x81;
const T_OUTPUT: u8 = 0x82;
const T_REPLAY_END: u8 = 0x83;
const T_EXIT: u8 = 0x84;
const T_NOTE: u8 = 0x85;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    // client → holder
    /// Must be the first frame on a connection. `replay` asks for the ring
    /// buffer before live output; `from` is the byte offset (in the child's
    /// total output) to replay from — `0` means everything the ring still has.
    Attach {
        replay: bool,
        from: u64,
    },
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Deliver a signal number to the child process.
    Signal(i32),
    /// Shut the holder down. A live child gets SIGHUP first.
    Close,

    // holder → client
    Hello(Hello),
    Output(Vec<u8>),
    /// Ring-buffer replay is done; what follows is live.
    ReplayEnd,
    /// The child has exited. Sent once per connection, after any output.
    Exit(Exit),
    /// A remark from whoever is relaying the stream (`rld` to a viewer:
    /// "x joined", "n bytes missing"). Never part of the child's output;
    /// the holder itself does not send these.
    Note(String),
}

/// What the holder knows about its session, sent on attach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub id: String,
    /// Pid of the child (the CLI), not the holder.
    pub pid: u32,
    pub holder_pid: u32,
    pub cols: u16,
    pub rows: u16,
    pub tty: bool,
    pub started_at: Timestamp,
    /// Set when the child already exited before this attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Exit>,
    /// Bytes the child has produced so far; live output continues here.
    #[serde(default)]
    pub total: u64,
    /// Offset the replay starts at. A client that asked for an earlier
    /// `from` lost `replay_from - from` bytes to the ring's capacity.
    #[serde(default)]
    pub replay_from: u64,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (tag, payload): (u8, Vec<u8>) = match self {
            Frame::Attach { replay, from } => {
                let mut p = vec![*replay as u8];
                p.extend_from_slice(&from.to_be_bytes());
                (T_ATTACH, p)
            }
            Frame::Input(b) => (T_INPUT, b.clone()),
            Frame::Resize { cols, rows } => {
                let mut p = cols.to_be_bytes().to_vec();
                p.extend_from_slice(&rows.to_be_bytes());
                (T_RESIZE, p)
            }
            Frame::Signal(n) => (T_SIGNAL, n.to_be_bytes().to_vec()),
            Frame::Close => (T_CLOSE, Vec::new()),
            Frame::Hello(h) => (T_HELLO, serde_json::to_vec(h).expect("hello serialises")),
            Frame::Output(b) => (T_OUTPUT, b.clone()),
            Frame::ReplayEnd => (T_REPLAY_END, Vec::new()),
            Frame::Exit(e) => (T_EXIT, serde_json::to_vec(e).expect("exit serialises")),
            Frame::Note(s) => (T_NOTE, s.as_bytes().to_vec()),
        };
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(tag);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    fn decode(tag: u8, p: Vec<u8>) -> io::Result<Frame> {
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("bad {what} frame"));
        Ok(match tag {
            // A 1-byte payload is an older client: replay from the start.
            T_ATTACH => Frame::Attach {
                replay: p.first().copied().unwrap_or(0) != 0,
                from: p
                    .get(1..9)
                    .map(|b| u64::from_be_bytes(b.try_into().unwrap()))
                    .unwrap_or(0),
            },
            T_INPUT => Frame::Input(p),
            T_RESIZE => {
                if p.len() != 4 {
                    return Err(bad("resize"));
                }
                Frame::Resize {
                    cols: u16::from_be_bytes([p[0], p[1]]),
                    rows: u16::from_be_bytes([p[2], p[3]]),
                }
            }
            T_SIGNAL => {
                if p.len() != 4 {
                    return Err(bad("signal"));
                }
                Frame::Signal(i32::from_be_bytes([p[0], p[1], p[2], p[3]]))
            }
            T_CLOSE => Frame::Close,
            T_HELLO => Frame::Hello(serde_json::from_slice(&p).map_err(|_| bad("hello"))?),
            T_OUTPUT => Frame::Output(p),
            T_REPLAY_END => Frame::ReplayEnd,
            T_EXIT => Frame::Exit(serde_json::from_slice(&p).map_err(|_| bad("exit"))?),
            T_NOTE => Frame::Note(String::from_utf8_lossy(&p).into_owned()),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown frame tag {other:#x}"),
                ));
            }
        })
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, f: &Frame) -> io::Result<()> {
    w.write_all(&f.encode()).await?;
    w.flush().await
}

/// A frame reader that survives a lost `select!`.
///
/// The half-read frame lives here rather than on the stack of the future,
/// which is the whole point: `select!` drops the losing branch's future, and
/// with it anything that future was holding. A 256 KiB scrollback replay
/// takes long enough to read that a ticker on another branch will interrupt
/// it, and before this existed that quietly desynchronised the connection —
/// every later read starting somewhere inside a payload.
pub struct FrameReader<R> {
    rd: R,
    /// The header, as far as it has been read.
    head: [u8; 5],
    /// How much of `head` is there.
    got: usize,
    /// The payload being gathered, once the header is whole.
    payload: Vec<u8>,
    /// How much of `payload` is there.
    filled: usize,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(rd: R) -> Self {
        Self {
            rd,
            head: [0u8; 5],
            got: 0,
            payload: Vec::new(),
            filled: 0,
        }
    }

    pub fn into_inner(self) -> R {
        self.rd
    }

    /// The next whole frame; `Ok(None)` on a clean EOF between frames.
    ///
    /// Cancel-safe: dropping the returned future between `await`s keeps
    /// whatever has been read so far, and the next call carries on.
    pub async fn next(&mut self) -> io::Result<Option<Frame>> {
        while self.got < 5 {
            let n = self.rd.read(&mut self.head[self.got..]).await?;
            if n == 0 {
                // Nothing at all: the sender hung up between frames, which is
                // how a connection is meant to end. Part of a header: it went
                // mid-frame, and that is a broken stream.
                return if self.got == 0 {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("stream ended {} bytes into a frame header", self.got),
                    ))
                };
            }
            self.got += n;
        }
        let len = u32::from_be_bytes([self.head[1], self.head[2], self.head[3], self.head[4]]);
        if len > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame of {len} bytes exceeds limit"),
            ));
        }
        if self.payload.len() != len as usize {
            self.payload = vec![0u8; len as usize];
        }
        while self.filled < self.payload.len() {
            let n = self.rd.read(&mut self.payload[self.filled..]).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("stream ended {} bytes into a {len}-byte payload", self.filled),
                ));
            }
            self.filled += n;
        }
        let tag = self.head[0];
        let payload = std::mem::take(&mut self.payload);
        self.got = 0;
        self.filled = 0;
        Frame::decode(tag, payload).map(Some)
    }
}

/// `Ok(None)` on a clean EOF between frames.
///
/// Not cancel-safe: a half-read frame is lost with the future. Use
/// [`FrameReader`] wherever the read shares a `select!`.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds limit"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Frame::decode(head[0], payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this type exists for: a big frame read inside a `select!`
    /// that loses to a ticker. `read_frame` drops what it had read along
    /// with its future, so the next read lands inside the payload and takes
    /// the child's own output for a frame header.
    ///
    /// The frame is fed in slices with a poll between each, so the read is
    /// cut short a known number of times rather than whenever the scheduler
    /// happens to interleave.
    #[tokio::test]
    async fn a_frame_survives_losing_a_select() {
        const CHUNK: usize = 1024;
        // Bytes that would read as an absurd length if taken for a header.
        let bytes = Frame::Output(vec![b'#'; 8 * CHUNK]).encode();
        let (mut w, r) = tokio::io::duplex(2 * CHUNK);
        let mut fr = FrameReader::new(r);

        let mut cut_short = 0;
        let mut sent = 0;
        let frame = loop {
            if sent < bytes.len() {
                let end = (sent + CHUNK).min(bytes.len());
                w.write_all(&bytes[sent..end]).await.unwrap();
                sent = end;
            }
            // A zero timeout polls the read once and then drops it — the
            // same cancellation `select!` performs, without a clock.
            match tokio::time::timeout(std::time::Duration::ZERO, fr.next()).await {
                Ok(f) => break f.unwrap().unwrap(),
                Err(_) => cut_short += 1,
            }
            assert!(cut_short < 1000, "the read never finished");
        };
        assert!(cut_short >= 8, "cut short {cut_short} times, expected one per slice");
        match frame {
            Frame::Output(b) => {
                assert_eq!(b.len(), 8 * CHUNK, "the whole payload, not a piece of it");
                assert!(b.iter().all(|c| *c == b'#'), "and none of it shifted");
            }
            other => panic!("expected the output frame, got {other:?}"),
        }

        // And the stream is still aligned behind it.
        write_frame(&mut w, &Frame::ReplayEnd).await.unwrap();
        assert_eq!(fr.next().await.unwrap(), Some(Frame::ReplayEnd));
        drop(w);
        assert_eq!(fr.next().await.unwrap(), None, "a clean end between frames");
    }

    /// A sender that goes mid-frame is a broken stream, not a clean end.
    #[tokio::test]
    async fn a_stream_cut_mid_frame_is_an_error() {
        let (mut w, r) = tokio::io::duplex(1024);
        let mut bytes = Frame::Output(vec![b'x'; 40]).encode();
        bytes.truncate(20);
        tokio::io::AsyncWriteExt::write_all(&mut w, &bytes).await.unwrap();
        drop(w);
        let mut fr = FrameReader::new(r);
        let e = fr.next().await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{e}");
        assert!(e.to_string().contains("into a 40-byte payload"), "{e}");
    }

    #[tokio::test]
    async fn every_frame_roundtrips() {
        let frames = vec![
            Frame::Attach { replay: true, from: 0 },
            Frame::Attach {
                replay: false,
                from: 12_345_678_901,
            },
            Frame::Input(b"hello\n".to_vec()),
            Frame::Resize { cols: 220, rows: 51 },
            Frame::Signal(15),
            Frame::Close,
            Frame::Hello(Hello {
                id: "k7x2m9qa".into(),
                pid: 42,
                holder_pid: 41,
                cols: 80,
                rows: 24,
                tty: true,
                started_at: Timestamp::UNIX_EPOCH,
                exit: None,
                total: 99,
                replay_from: 10,
            }),
            Frame::Output(vec![0, 1, 2, 255]),
            Frame::ReplayEnd,
            Frame::Exit(Exit {
                code: 3,
                signal: Some("SIGTERM".into()),
                at: Timestamp::UNIX_EPOCH,
            }),
            Frame::Note("fabio@mac joined".into()),
        ];
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        for f in &frames {
            write_frame(&mut a, f).await.unwrap();
        }
        drop(a);
        for want in &frames {
            let got = read_frame(&mut b).await.unwrap().unwrap();
            assert_eq!(&got, want);
        }
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error_not_eof() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[T_INPUT, 0, 0, 0, 10, 1, 2]).await.unwrap();
        drop(a);
        assert!(read_frame(&mut b).await.is_err());
    }
}

//! The terminal screen: a run's live output rendered through a `vt100`
//! screen, watched first, joined on the first keypress (J7).
//!
//! Two daemon connections at most: the watch connection carries output for
//! as long as this view lasts — which outlives stepping back to the list, so
//! coming back is instant and keeps its place — and a join connection is
//! opened on demand, carrying keystrokes and resizes. That way escalating to
//! join never restarts the screen, and letting go never loses it.

use crate::attach::client_name;
use crate::client::Client;
use relay_core::Home;
use relay_proto::methods::{AttachInfo, AttachParams, RunAttach};
use relay_proto::rpc::RpcError;
use relay_proto::types::AttachMode;
use rl_session::proto::{Frame, read_frame, write_frame};
use std::io;
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const SCROLLBACK: u64 = 256 * 1024;
/// Rows kept above the screen, to read back through. The daemon replays
/// bytes; without somewhere to put them only the last screenful survived.
const SCROLLBACK_ROWS: usize = 10_000;

pub struct TermView {
    pub id: String,
    pub info: AttachInfo,
    rd: BufReader<OwnedReadHalf>,
    _wr: OwnedWriteHalf,
    join: Option<(BufReader<OwnedReadHalf>, OwnedWriteHalf)>,
    parser: vt100::Parser,
    /// The pane we draw into; the pty is resized to it once joined.
    pane: (u16, u16),
    /// Latest word from the daemon and when it arrived, for the footer.
    pub note: Option<(String, std::time::Instant)>,
    /// Who holds the keyboard when our join was refused.
    pub busy_with: Option<String>,
    pub ended: Option<i32>,
    /// How far back the view is, in rows; 0 is the live screen.
    scroll: usize,
    /// The watch stream is over. An ended run's output is still worth
    /// reading, so the view stays; there is just nothing more to poll.
    pub over: bool,
}

impl TermView {
    /// Opens the watch connection. `pane` is (cols, rows) of the drawing area.
    pub async fn open(home: &Home, id: &str, pane: (u16, u16)) -> anyhow::Result<Self> {
        let mut c = Client::connect(home).await?;
        let info = c
            .call::<RunAttach>(AttachParams {
                id: id.to_string(),
                mode: AttachMode::Watch,
                client: client_name(),
                scrollback: SCROLLBACK,
                take: false,
            })
            .await?;
        let (rd, wr) = c.into_parts();
        let (cols, rows) = if info.live && info.cols > 0 {
            (info.cols, info.rows)
        } else {
            pane
        };
        Ok(Self {
            id: id.to_string(),
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            info,
            rd,
            _wr: wr,
            join: None,
            pane,
            note: None,
            busy_with: None,
            ended: None,
            scroll: 0,
            over: false,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn joined(&self) -> bool {
        self.join.is_some()
    }

    /// Next frame from the watch connection. `Ok(false)` when the stream is over.
    pub async fn pump(&mut self) -> io::Result<bool> {
        match read_frame(&mut self.rd).await? {
            None => {
                self.over = true;
                Ok(false)
            }
            Some(Frame::Output(b)) => {
                self.parser.process(&b);
                // New output must not yank a reader back to the bottom.
                if self.scroll > 0 {
                    self.parser.screen_mut().set_scrollback(self.scroll);
                }
                Ok(true)
            }
            Some(Frame::Note(s)) => {
                self.note = Some((s, std::time::Instant::now()));
                Ok(true)
            }
            Some(Frame::Exit(e)) => {
                self.ended = Some(e.code);
                let how = match &e.signal {
                    Some(s) => format!("run ended ({s}, exit {})", e.code),
                    None => format!("run ended (exit {})", e.code),
                };
                self.note = Some((how, std::time::Instant::now()));
                Ok(true)
            }
            Some(_) => Ok(true),
        }
    }

    /// Escalates to join. `Ok(false)` if someone else holds the keyboard
    /// (see `busy_with`).
    pub async fn join(&mut self, home: &Home, take: bool) -> anyhow::Result<bool> {
        if self.join.is_some() || self.ended.is_some() || !self.info.live {
            return Ok(self.join.is_some());
        }
        let mut c = Client::connect(home).await?;
        let attempt = c
            .call::<RunAttach>(AttachParams {
                id: self.id.clone(),
                mode: AttachMode::Join,
                client: client_name(),
                scrollback: 0,
                take,
            })
            .await;
        match attempt {
            Ok(_) => {}
            Err(e) => {
                if let Some(rpc) = e.downcast_ref::<RpcError>()
                    && rpc.code == RpcError::BUSY
                {
                    self.busy_with = Some(
                        rpc.message
                            .split("joined by ")
                            .nth(1)
                            .and_then(|s| s.split(';').next())
                            .unwrap_or("someone")
                            .to_string(),
                    );
                    return Ok(false);
                }
                return Err(e);
            }
        }
        let (rd, mut wr) = c.into_parts();
        // The pty follows our pane from now on.
        let (cols, rows) = self.pane;
        write_frame(&mut wr, &Frame::Resize { cols, rows }).await?;
        self.parser.screen_mut().set_size(rows, cols);
        // Taking the keyboard means being where the run is, not where you
        // were reading.
        self.parser.screen_mut().set_scrollback(0);
        self.scroll = 0;
        self.join = Some((rd, wr));
        self.busy_with = None;
        Ok(true)
    }

    pub async fn input(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        if let Some((_, wr)) = &mut self.join {
            write_frame(wr, &Frame::Input(bytes)).await?;
        }
        Ok(())
    }

    /// The drawing area changed. Joined: the pty follows; watching: we just
    /// clip differently.
    pub async fn resized(&mut self, pane: (u16, u16)) -> io::Result<()> {
        self.pane = pane;
        if let Some((_, wr)) = &mut self.join {
            let (cols, rows) = pane;
            write_frame(wr, &Frame::Resize { cols, rows }).await?;
            self.parser.screen_mut().set_size(rows, cols);
            if self.scroll > 0 {
                self.parser.screen_mut().set_scrollback(self.scroll);
            }
        }
        Ok(())
    }

    pub fn app_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    /// Moves the view back through what scrolled off. Positive rows go
    /// further back; it stops at the oldest line kept, and at the live
    /// screen. vt100 clamps, so its answer is the one we keep.
    pub fn scroll_by(&mut self, rows: isize) {
        let want = (self.scroll as isize).saturating_add(rows).max(0) as usize;
        self.parser.screen_mut().set_scrollback(want);
        self.scroll = self.parser.screen().scrollback();
    }

    /// How far back the view is, in rows.
    pub fn scrolled(&self) -> usize {
        self.scroll
    }

    /// Gives the keyboard back, still watching. Dropping the connection is
    /// how the daemon is told: it releases the run to whoever asks next.
    pub fn unjoin(&mut self) {
        self.join = None;
    }
}

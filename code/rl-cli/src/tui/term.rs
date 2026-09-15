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
use rl_core::Home;
use rl_proto::methods::{AttachInfo, AttachParams, RunAttach};
use rl_proto::types::AttachMode;
use rl_session::proto::{Frame, read_frame};
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
    parser: vt100::Parser,
    /// The area we draw into, for clipping the re-render.
    pane: (u16, u16),
    /// Latest word from the daemon and when it arrived, for the footer.
    pub note: Option<(String, std::time::Instant)>,
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
            pane,
            note: None,
            ended: None,
            scroll: 0,
            over: false,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
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

    /// The drawing area changed. Watching only clips differently — the run's
    /// own size is the daemon's business, not ours; taking the keyboard is a
    /// separate handover (`raw::attach`) that resizes the pty itself.
    pub async fn resized(&mut self, pane: (u16, u16)) -> io::Result<()> {
        self.pane = pane;
        Ok(())
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
}

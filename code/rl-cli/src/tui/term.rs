//! The terminal screen: an ended run's output rendered through a `vt100`
//! screen, to read (J7).
//!
//! One daemon connection, a watcher's: it carries the output for as long as
//! this view lasts, which outlives stepping back to the list, so coming back
//! is instant and keeps its place.

use crate::attach::client_name;
use crate::client::Client;
use futures::FutureExt;
use rl_core::Home;
use rl_proto::methods::{AttachInfo, AttachParams, RunAttach};
use rl_proto::types::AttachMode;
use rl_session::proto::{Frame, FrameReader};
use std::io;
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const SCROLLBACK: u64 = 256 * 1024;
/// Rows kept above the screen, to read back through. What the daemon sends
/// carries the run's history above its screen; without somewhere to put it
/// only the last screenful survived.
const SCROLLBACK_ROWS: usize = 10_000;

pub struct TermView {
    pub id: String,
    pub info: AttachInfo,
    /// Cancel-safe: the panel polls this inside a `select!` beside keys,
    /// the refresh tick and every other source, so a half-read frame has to
    /// survive losing that race.
    rd: FrameReader<BufReader<OwnedReadHalf>>,
    _wr: OwnedWriteHalf,
    parser: vt100::Parser,
    /// Draws the box-drawing set as the characters it stands for. `vt100`
    /// hands `ESC ( 0` to its unhandled hook, so a prompt that rules a line
    /// off would otherwise reach the screen spelling `qqqqqq`.
    graphics: rl_session::screen::Graphics,
    /// The area we draw into, for clipping the re-render.
    pane: (u16, u16),
    /// Latest word from the daemon and when it arrived, for the footer.
    pub note: Option<(String, std::time::Instant)>,
    pub ended: Option<i32>,
    /// The stream went without an exit: the run was not seen to end, so what
    /// is on screen is the last that was heard, not the last that happened.
    pub lost: bool,
    /// How far back the view is, in rows; 0 is the live screen.
    scroll: usize,
    /// The watch stream is over. An ended run's output is still worth
    /// reading, so the view stays; there is just nothing more to poll.
    pub over: bool,
    /// A run on pipes: its bare `\n`s get their `\r`s, or the screen climbs
    /// down as a staircase.
    newlines: Option<crate::attach::Newlines>,
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
                terminal: Some(crate::attach::terminal()),
            })
            .await?;
        let (rd, wr) = c.into_parts();
        let rd = FrameReader::new(rd);
        let newlines = (!info.tty).then(crate::attach::Newlines::default);
        let (cols, rows) = drawn_at(info.tty, (info.cols, info.rows), pane);
        Ok(Self {
            id: id.to_string(),
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            graphics: rl_session::screen::Graphics::new(),
            info,
            rd,
            _wr: wr,
            pane,
            note: None,
            ended: None,
            lost: false,
            scroll: 0,
            over: false,
            newlines,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Next frame from the watch connection. `Ok(false)` when the stream is over.
    pub async fn pump(&mut self) -> io::Result<bool> {
        match self.rd.next().await? {
            None => {
                self.over = true;
                Ok(false)
            }
            Some(frame) => {
                self.apply(frame);
                Ok(true)
            }
        }
    }

    /// Takes in every frame already buffered, without awaiting more, and says
    /// whether the stream is still live, as [`pump`](Self::pump) does. An
    /// inline agent repaints by climbing the screen and rewriting a line at a
    /// time — Gemini does this on each keystroke, erasing and redrawing its
    /// prompt box — and each step arrives as its own frame. Draining the burst
    /// before the panel redraws means one whole repaint is drawn, not every
    /// half-erased step of it, which is what showed as flicker. `next` is
    /// cancel-safe, so a frame still arriving is left whole for the next pump.
    pub fn drain_ready(&mut self) -> io::Result<bool> {
        loop {
            match self.rd.next().now_or_never() {
                Some(Ok(Some(frame))) => self.apply(frame),
                Some(Ok(None)) => {
                    self.over = true;
                    return Ok(false);
                }
                Some(Err(e)) => return Err(e),
                // Nothing more is sitting in the buffer yet.
                None => return Ok(true),
            }
        }
    }

    /// Takes one frame into the view: output onto the screen, a note or an
    /// exit into the footer.
    fn apply(&mut self, frame: Frame) {
        match frame {
            Frame::Output(b) => {
                let b = match self.newlines.as_mut() {
                    Some(n) => n.fix(&b),
                    None => b,
                };
                self.parser.process(&self.graphics.rewrite(&b));
                // New output must not yank a reader back to the bottom.
                if self.scroll > 0 {
                    self.parser.screen_mut().set_scrollback(self.scroll);
                }
            }
            Frame::Note(s) => {
                self.note = Some((s, std::time::Instant::now()));
            }
            Frame::Exit(e) => {
                self.ended = Some(e.code);
                let how = match &e.signal {
                    Some(s) => format!("run ended ({s}, exit {})", e.code),
                    None => format!("run ended (exit {})", e.code),
                };
                self.note = Some((how, std::time::Instant::now()));
            }
            _ => {}
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

/// The size the view's screen is kept at. A terminal run's, live or ended,
/// arrives drawn for its own size with cursor moves to absolute places, so
/// it is kept at that size and the pane clips it; drawn into a smaller one
/// its rows pile onto the last. A pipes run's lines wrap at the pane.
fn drawn_at(tty: bool, run: (u16, u16), pane: (u16, u16)) -> (u16, u16) {
    if tty && run.0 > 0 && run.1 > 0 { run } else { pane }
}

#[cfg(test)]
impl TermView {
    /// A view reading from an arbitrary stream rather than a daemon, for tests.
    fn piped(rd: OwnedReadHalf, wr: OwnedWriteHalf, cols: u16, rows: u16) -> Self {
        use rl_proto::types::{Run, RunKind};
        let now = "2020-01-01T00:00:00Z".parse().unwrap();
        let run = Run::new("test", RunKind::Agent, "test", vec![], "/w", "/w", now);
        Self {
            id: "test".into(),
            info: AttachInfo {
                run,
                cols,
                rows,
                tty: true,
                live: true,
                joiner: None,
                journaled: 0,
                replay_from: 0,
            },
            rd: FrameReader::new(BufReader::new(rd)),
            _wr: wr,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            graphics: rl_session::screen::Graphics::new(),
            pane: (cols, rows),
            note: None,
            ended: None,
            lost: false,
            scroll: 0,
            over: false,
            newlines: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// The flicker fix: an inline agent repaints by rewriting a line over and
    /// over, each rewrite its own frame. One `drain_ready` takes in the whole
    /// buffered burst, so the panel draws the finished frame, not a step of it.
    #[tokio::test]
    async fn a_burst_of_buffered_frames_is_drained_in_one_pass() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (a_rd, a_wr) = a.into_split();
        let (_b_rd, mut b_wr) = b.into_split();
        let mut view = TermView::piped(a_rd, a_wr, 20, 3);

        // Three repaints of the same line, as an inline agent would send them.
        for s in ["one", "two", "three"] {
            let frame = Frame::Output(format!("\r\x1b[2K{s}").into_bytes()).encode();
            b_wr.write_all(&frame).await.unwrap();
        }
        b_wr.flush().await.unwrap();

        // The loop wakes on the first frame, as it does in `main_loop`.
        assert!(view.pump().await.unwrap(), "stream is live");
        // The other two are already buffered: one drain takes them both, and
        // the stream is still open, so nothing more is ready right now.
        assert!(view.drain_ready().unwrap(), "stream is still live");
        let row0 = view.screen().rows(0, 20).next().unwrap();
        assert_eq!(row0.trim_end(), "three", "only the final repaint is left");
    }

    /// A stream that ends mid-drain is reported over, not swallowed.
    #[tokio::test]
    async fn a_drain_that_meets_the_end_says_so() {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (a_rd, a_wr) = a.into_split();
        let (_b_rd, mut b_wr) = b.into_split();
        let mut view = TermView::piped(a_rd, a_wr, 20, 3);

        b_wr.write_all(&Frame::Output(b"hi".to_vec()).encode()).await.unwrap();
        b_wr.flush().await.unwrap();
        drop(b_wr);
        drop(_b_rd);

        assert!(view.pump().await.unwrap());
        assert!(!view.drain_ready().unwrap(), "the end is reached");
        assert!(view.over);
        assert_eq!(view.screen().rows(0, 20).next().unwrap().trim_end(), "hi");
    }

    /// An ended terminal run is drawn by rld at the run's size: a pane-sized
    /// screen took the drawing's moves to rows it does not have.
    #[test]
    fn a_terminal_runs_screen_is_kept_at_the_runs_size() {
        let run = (100, 30);
        let pane = (60, 12);
        assert_eq!(drawn_at(true, run, pane), run);
        assert_eq!(drawn_at(false, run, pane), pane, "pipes wrap at the pane");
        assert_eq!(drawn_at(true, (0, 0), pane), pane, "no size known");

        let mut out = Vec::new();
        for i in 0..30 {
            out.extend_from_slice(format!("row {i:02}\r\n").as_bytes());
        }
        out.extend_from_slice(b"last");
        let drawn = rl_session::screen::Screen::render(&out, run.0, run.1);
        let (cols, rows) = drawn_at(true, run, pane);
        let mut p = vt100::Parser::new(rows, cols, 1000);
        p.process(&drawn);
        let shown: Vec<String> = p.screen().rows(0, cols).map(|r| r.trim_end().to_string()).collect();
        assert_eq!(shown.first().map(String::as_str), Some("row 01"));
        assert_eq!(shown.last().map(String::as_str), Some("last"));
    }
}

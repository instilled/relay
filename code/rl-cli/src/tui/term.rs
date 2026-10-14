//! An ended run's output, read as a static page (J7).
//!
//! The daemon answers an attach to an ended run with its screen drawn afresh
//! followed by the exit, whatever the mode asked for. This reads that to the
//! exit in one go, lets the connection go, and keeps the bytes: nothing more
//! is coming, and a resize of the user's own terminal only re-wraps them.

use crate::attach::client_name;
use crate::client::Client;
use rl_core::Home;
use rl_proto::methods::{AttachParams, RunAttach};
use rl_session::proto::{Frame, FrameReader};
use tokio::io::BufReader;

/// Rows kept above the screen, to read back through. What the daemon sends
/// carries the run's history above its screen; without somewhere to put it
/// only the last screenful survived.
const SCROLLBACK_ROWS: usize = 10_000;

pub struct Output {
    pub id: String,
    /// The run's title when it was opened, for the head if the list no
    /// longer has it.
    pub title: String,
    parser: vt100::Parser,
    /// Everything the daemon sent as output, as it came, so a resize can
    /// draw it again at the new size.
    held: Vec<u8>,
    /// Whether the run was on a terminal, and the size it was drawn for.
    tty: bool,
    run: (u16, u16),
    /// The area we draw into, for clipping the re-render.
    pane: (u16, u16),
    ended: Option<i32>,
    /// How far back the view is, in rows; 0 is the bottom of the screen.
    scroll: usize,
}

impl Output {
    /// Reads run `id`'s ended output. `pane` is (cols, rows) of the drawing
    /// area. The connection is dropped once the exit is read.
    pub async fn open(home: &Home, id: &str, pane: (u16, u16)) -> anyhow::Result<Self> {
        let mut c = Client::connect(home).await?;
        let info = c
            .call::<RunAttach>(AttachParams {
                id: id.to_string(),
                client: client_name(),
                scrollback: super::SCROLLBACK,
                take: false,
                terminal: Some(crate::attach::terminal()),
            })
            .await?;
        let (rd, _wr) = c.into_parts();
        let mut rd = FrameReader::new(BufReader::new(rd));
        let mut out = Self::new(id, &info.run.title, info.tty, (info.cols, info.rows), pane);
        let mut frames = Vec::new();
        while let Some(frame) = rd.next().await? {
            let done = matches!(frame, Frame::Exit(_));
            frames.push(frame);
            if done {
                break;
            }
        }
        out.ingest(frames);
        Ok(out)
    }

    fn new(id: &str, title: &str, tty: bool, run: (u16, u16), pane: (u16, u16)) -> Self {
        let (cols, rows) = drawn_at(tty, run, pane);
        Self {
            id: id.to_string(),
            title: title.to_string(),
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            held: Vec::new(),
            tty,
            run,
            pane,
            ended: None,
            scroll: 0,
        }
    }

    /// An ended run's page, as the daemon would have sent it, for the
    /// panel's tests to put up.
    #[cfg(test)]
    pub(super) fn ended_with(id: &str, title: &str, text: &str, code: i32, pane: (u16, u16)) -> Self {
        let mut o = Self::new(id, title, false, (0, 0), pane);
        o.ingest([
            Frame::Output(text.as_bytes().to_vec()),
            Frame::Exit(rl_session::exit::Exit {
                code,
                signal: None,
                at: jiff::Timestamp::now(),
            }),
        ]);
        o
    }

    /// Takes in what the daemon sent: output is drawn, the exit is
    /// remembered, and anything after the exit is ignored.
    fn ingest(&mut self, frames: impl IntoIterator<Item = Frame>) {
        for frame in frames {
            match frame {
                Frame::Output(b) => self.held.extend_from_slice(&b),
                Frame::Exit(e) => {
                    self.ended = Some(e.code);
                    break;
                }
                _ => {}
            }
        }
        self.render();
    }

    /// Draws the held bytes onto a fresh screen the size the view keeps.
    fn render(&mut self) {
        let (cols, rows) = drawn_at(self.tty, self.run, self.pane);
        self.parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
        // A run on pipes: its bare `\n`s get their `\r`s, or the screen
        // climbs down as a staircase. `vt100` hands `ESC ( 0` to its
        // unhandled hook, so the box-drawing set is drawn as the characters
        // it stands for, or a ruled line would reach the screen as `qqqqqq`.
        let mut newlines = (!self.tty).then(crate::attach::Newlines::default);
        let mut graphics = rl_session::screen::Graphics::new();
        let bytes = match newlines.as_mut() {
            Some(n) => n.fix(&self.held),
            None => self.held.clone(),
        };
        self.parser.process(&graphics.rewrite(&bytes));
        self.parser.screen_mut().set_scrollback(self.scroll);
        self.scroll = self.parser.screen().scrollback();
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// The exit code the run ended with, once its exit has been read.
    pub fn ended(&self) -> Option<i32> {
        self.ended
    }

    /// Nothing was recorded: the run printed no output.
    pub fn empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The drawing area changed, by the user resizing their own terminal:
    /// the held output is drawn again at the new size.
    pub fn relayout(&mut self, pane: (u16, u16)) {
        if pane == self.pane {
            return;
        }
        self.pane = pane;
        self.render();
    }

    /// Moves the view back through what scrolled off. Positive rows go
    /// further back; it stops at the oldest line kept, and at the bottom.
    /// vt100 clamps, so its answer is the one we keep.
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

/// The size the view's screen is kept at. A terminal run's arrives drawn
/// for its own size with cursor moves to absolute places, so it is kept at
/// that size and the pane clips it; drawn into a smaller one its rows pile
/// onto the last. A pipes run's lines wrap at the pane.
fn drawn_at(tty: bool, run: (u16, u16), pane: (u16, u16)) -> (u16, u16) {
    if tty && run.0 > 0 && run.1 > 0 { run } else { pane }
}

#[cfg(test)]
mod tests {
    use super::{Output, drawn_at};
    use rl_session::exit::Exit;
    use rl_session::proto::Frame;

    fn exit(code: i32) -> Frame {
        Frame::Exit(Exit {
            code,
            signal: None,
            at: jiff::Timestamp::now(),
        })
    }

    /// An ended run's frames are all there is: both outputs are on the
    /// screen and the exit code is kept.
    #[test]
    fn an_ended_runs_frames_make_the_page() {
        let mut o = Output::new("r1", "t", false, (0, 0), (40, 6));
        o.ingest([
            Frame::Output(b"first\n".to_vec()),
            Frame::ReplayEnd,
            Frame::Output(b"second\n".to_vec()),
            exit(2),
        ]);
        let shown = o.screen().contents();
        assert!(shown.contains("first") && shown.contains("second"), "{shown}");
        assert_eq!(o.ended(), Some(2));
        assert!(!o.empty());
    }

    /// No output: nothing was recorded, and no exit was heard.
    #[test]
    fn no_frames_is_an_empty_page() {
        let mut o = Output::new("r1", "t", false, (0, 0), (40, 6));
        o.ingest(Vec::<Frame>::new());
        assert!(o.empty());
        assert_eq!(o.ended(), None);
        assert!(o.screen().contents().trim().is_empty());
    }

    /// The user resizing their terminal re-wraps what was read.
    #[test]
    fn a_resize_draws_the_held_output_again() {
        let mut o = Output::new("r1", "t", false, (0, 0), (40, 6));
        o.ingest([Frame::Output(b"abcdefghij\n".to_vec()), exit(0)]);
        assert_eq!(o.screen().rows(0, 40).next().unwrap().trim_end(), "abcdefghij");
        o.relayout((5, 6));
        assert_eq!(o.screen().rows(0, 5).next().unwrap().trim_end(), "abcde");
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

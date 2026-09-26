//! Irori's own log, kept in memory as well as on stdout, so the Settings page can show it.
//!
//! Logs go where they always went: stdout, which is journald, Docker, a file, or the terminal
//! whoever started the process. On a box Irori starts itself — a container, a systemd unit — there
//! is no terminal to read them, and "what did it say?" then has nowhere to look. So the same
//! lines are kept a second time, in a ring, and the page reads that.
//!
//! This is a window onto a running instance for a person looking at a problem now, deliberately
//! not a log archive: the recent past, in memory, lost on restart. `irori logs` reading a file
//! across restarts is a different job and still to come (ROADMAP D18).

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// How many lines the view can still show. Deliberately the same size as what an extension's own
/// output keeps (`irori_core::LOG_LINES_KEPT`), so the two windows hold about the same amount and
/// neither can be accused of keeping more of one kind of line than of the other.
pub const LINES_KEPT: usize = 400;

/// A line longer than this is cut short with an ellipsis. A log line is a sentence a person reads;
/// one runaway value — a whole device payload dumped into a message field — shouldn't push every
/// other line out of the window. The same cap the core keeps for an extension's output.
const LINE_MAX: usize = 4096;

/// The last lines this process wrote, oldest first. Cheap to share, so the writer that fills it and
/// the handler that serves it hold the same one.
#[derive(Debug, Default)]
pub struct Log {
    lines: Mutex<VecDeque<String>>,
}

impl Log {
    /// Keeps one line, dropping the oldest once [`LINES_KEPT`] are held.
    ///
    /// Blank lines are dropped rather than kept: the window separates nothing, and the blank ones
    /// a formatted log carries are padding a person scrolls past.
    pub fn keep(&self, line: &str) {
        let line = without_colour(line.trim_end());
        if line.is_empty() {
            return;
        }
        let mut line = line;
        if line.len() > LINE_MAX {
            line.truncate(
                (0..=LINE_MAX)
                    .rev()
                    .find(|at| line.is_char_boundary(*at))
                    .unwrap_or(0),
            );
            line.push('…');
        }
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() >= LINES_KEPT {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// What this process has said lately, oldest first. Empty before anything has been logged,
    /// which is a real answer rather than a failure — see the endpoint in `server.rs`.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

/// The writer the log subscriber formats into: what Irori prints, and a copy kept for the page.
///
/// One writer rather than two subscriber layers, so what a person reads in a terminal and what the
/// page shows are the same lines and cannot drift apart — the same level, the same fields, the
/// same filtering. Only the colour differs, and only because colour codes are for a terminal:
/// `Log::keep` strips them (see [`without_colour`]), while stdout keeps whatever the subscriber was
/// told to write.
///
/// Lines are reassembled from the byte stream before being kept, because the formatter is free to
/// write a line in several `write` calls; splitting on a call boundary would show fragments.
#[derive(Clone, Debug)]
pub struct Tee {
    log: Arc<Log>,
    /// The bytes of a line not yet terminated by a newline, held until the rest of it arrives.
    pending: Vec<u8>,
}

impl Tee {
    /// Keeps a copy of everything written to `log`, while still writing it to stdout.
    pub fn new(log: Arc<Log>) -> Self {
        Self {
            log,
            pending: Vec::new(),
        }
    }
}

impl io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write_all(buf)?;
        // Bytes rather than a `String`: the formatter's own writes are pieces of a line, and one
        // piece can end mid-character. Decoding only once a whole line has arrived can't split a
        // character that way — a newline is ASCII, so it can't fall inside one either.
        self.pending.extend_from_slice(buf);
        while let Some(at) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            self.log.keep(&String::from_utf8_lossy(&line));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // A last line with no newline after it — the formatter writes one per event and ends each
        // with a newline, but a truncated final write shouldn't lose its text silently.
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.log.keep(&String::from_utf8_lossy(&line));
        }
        io::stdout().flush()
    }
}

impl<'a> MakeWriter<'a> for Tee {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A line without its terminal colour codes.
///
/// The same guard the core keeps on an extension's own output (`irori_core`, `log_line`): stdout
/// is colourised when it's a terminal, and escape sequences belong in a terminal, not in text a
/// browser shows inside a `<pre>`. Duplicated rather than shared because `irori-core` is the core's
/// crate and a text-cleaning helper for the binary's log is not part of its surface.
fn without_colour(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // `ESC [ … <final>`: parameters and separators, ended by any letter or `@`-range byte.
        if chars.next() != Some('[') {
            continue;
        }
        for c in chars.by_ref() {
            if !matches!(c, '0'..='9' | ';' | ':' | '?') {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_log_has_said_nothing() {
        assert!(Log::default().lines().is_empty());
    }

    #[test]
    fn lines_are_kept_in_the_order_they_were_written() {
        let log = Log::default();
        log.keep("first");
        log.keep("second");
        assert_eq!(log.lines(), vec!["first", "second"]);
    }

    #[test]
    fn blank_lines_are_not_worth_a_line_in_the_window() {
        let log = Log::default();
        log.keep("");
        log.keep("   ");
        log.keep("\n");
        log.keep("said something");
        assert_eq!(log.lines(), vec!["said something"]);
    }

    #[test]
    fn the_oldest_line_goes_when_the_window_is_full() {
        let log = Log::default();
        for n in 0..=LINES_KEPT {
            log.keep(&format!("line {n}"));
        }
        let lines = log.lines();
        // One past the cap, so the first is the one that fell out and the last is still there.
        let last = format!("line {LINES_KEPT}");
        assert_eq!(lines.len(), LINES_KEPT);
        assert_eq!(lines.first().map(String::as_str), Some("line 1"));
        assert_eq!(lines.last().map(String::as_str), Some(last.as_str()));
    }

    #[test]
    fn a_runaway_line_is_cut_rather_than_asked_to_push_the_others_out() {
        let log = Log::default();
        // Multi-byte characters right at the cap, so a cut that ignored character boundaries
        // would panic here instead of quietly writing half of one.
        let huge = "é".repeat(LINE_MAX);
        log.keep(&huge);
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with('…'));
        // The whole of the text up to the cap, one byte at a time fewer where a character
        // straddles it, and no invalid UTF-8 left behind.
        assert!(lines[0].len() <= LINE_MAX + '…'.len_utf8());
    }

    #[test]
    fn colour_codes_are_kept_out_of_the_text_the_page_shows() {
        // What the formatter writes for a warning on a terminal: the message wrapped in the
        // colours it chose for the level.
        let coloured = "\u{1b}[33mWARN\u{1b}[0m something needed saying";
        let log = Log::default();
        log.keep(coloured);
        assert_eq!(log.lines(), vec!["WARN something needed saying"]);
    }

    #[test]
    fn a_line_split_across_writes_is_kept_whole() {
        // A formatter is free to write one line in several `write` calls, and a cut at a call
        // boundary would show fragments in the window.
        let log = Arc::new(Log::default());
        let mut tee = Tee::new(Arc::clone(&log));
        for piece in ["2026-09-26T10:00:00Z  INFO irori", " is ready", "\nnext\n"] {
            tee.write_all(piece.as_bytes()).expect("stdout accepts this");
        }
        assert_eq!(
            log.lines(),
            vec!["2026-09-26T10:00:00Z  INFO irori is ready", "next"]
        );
    }

    #[test]
    fn a_character_split_across_writes_is_not_mangled() {
        // "é" is two bytes, so a line cut between them is a split character, not a split word.
        // Decoding each piece on its own would write a replacement character in its place.
        let log = Arc::new(Log::default());
        let mut tee = Tee::new(Arc::clone(&log));
        let bytes = "café\n".as_bytes();
        tee.write_all(&bytes[..4]).expect("stdout accepts this");
        tee.write_all(&bytes[4..]).expect("stdout accepts this");
        assert_eq!(log.lines(), vec!["café"]);
    }

    #[test]
    fn a_last_line_without_a_newline_is_kept_when_the_writer_is_flushed() {
        let log = Arc::new(Log::default());
        let mut tee = Tee::new(Arc::clone(&log));
        tee.write_all(b"unterminated").expect("stdout accepts this");
        assert!(
            log.lines().is_empty(),
            "not yet: the rest of the line could still be coming"
        );
        tee.flush().expect("stdout flushes");
        assert_eq!(log.lines(), vec!["unterminated"]);
    }
}

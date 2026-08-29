//! In-memory ring buffer for the dashboard's Logs tab.
//!
//! The Tauri process already writes formatted tracing output to stdout. This
//! module wraps that same output in a `MakeWriter` so a bounded copy of the
//! recent lines is kept for the webview without changing the terminal view.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::writer::MakeWriter;

#[derive(Debug, Default)]
struct LogState {
    lines: VecDeque<String>,
    pending: String,
}

/// A bounded, lock-protected buffer of recent log lines.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    state: Arc<Mutex<LogState>>,
    max_lines: usize,
}

impl LogBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(LogState::default())),
            max_lines: max_lines.max(1),
        }
    }

    /// Snapshot of the current lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.lines.iter().cloned().collect()
    }

    /// Feed raw bytes written by the tracing formatter.
    fn push_bytes(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending.push_str(&text);
        while let Some(pos) = state.pending.find('\n') {
            let mut line = state.pending[..pos].to_string();
            state.pending.drain(..=pos);
            if line.ends_with('\r') {
                line.pop();
            }
            let line = strip_ansi(&line);
            if !line.is_empty() {
                state.lines.push_back(line);
                while state.lines.len() > self.max_lines {
                    state.lines.pop_front();
                }
            }
        }
    }
}

/// Remove ANSI/VT100 escape sequences so the webview gets plain text.
///
/// The terminal writer keeps the original coloured output; this buffer only
/// stores the human-readable version.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }

        match chars.peek().copied() {
            // CSI: ESC [ parameter* intermediate* final
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    let is_parameter = matches!(c, '\x30'..='\x3f');
                    let is_intermediate = matches!(c, '\x20'..='\x2f');
                    if !is_parameter && !is_intermediate {
                        // Final byte is part of the escape sequence.
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL or ST
            Some(']') => {
                chars.next();
                for c in chars.by_ref() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Two-character escape (e.g. ESC c, ESC 7).
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }

    out
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buffer: self,
            stdout: io::stdout(),
        }
    }
}

/// A `Write` handle that mirrors tracing output to both the terminal and the
/// in-memory log buffer.
pub struct LogWriter<'a> {
    buffer: &'a LogBuffer,
    stdout: io::Stdout,
}

impl Write for LogWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.push_bytes(buf);
        self.stdout.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_colour_and_style_sequences() {
        let raw = "\u{1b}[2m2026-08-29T14:46:51Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m building \u{1b}[3mbypass_proxy\u{1b}[0m\u{1b}[2m=true\u{1b}[0m";
        assert_eq!(
            strip_ansi(raw),
            "2026-08-29T14:46:51Z  INFO building bypass_proxy=true"
        );
    }

    #[test]
    fn buffer_stores_plain_text_lines() {
        let buffer = LogBuffer::new(10);
        buffer.push_bytes(b"\x1b[32m INFO\x1b[0m proxy listening on http://127.0.0.1:8787\n");
        assert_eq!(
            buffer.lines(),
            vec![" INFO proxy listening on http://127.0.0.1:8787"]
        );
    }
}

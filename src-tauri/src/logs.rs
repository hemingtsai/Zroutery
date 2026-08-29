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
            if !line.is_empty() {
                state.lines.push_back(line);
                while state.lines.len() > self.max_lines {
                    state.lines.pop_front();
                }
            }
        }
    }
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

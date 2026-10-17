//! The stdout/stderr split.
//!
//! **stdout is data, stderr is diagnostics.** In `--json` mode stdout carries one JSON value and
//! nothing else; human text and notes never go there. In the default human mode stdout carries the
//! human summary of the same information. Notes, import warnings and unsupported-operation reports
//! are always diagnostics, so they are always on stderr.

use std::io::{self, Write};

use serde_json::Value;

use crate::CliError;

/// Where a command writes.
pub struct Out<'a> {
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
    json: bool,
    /// The first real write failure (a closed pipe is normal and ignored).
    write_error: Option<String>,
}

impl<'a> Out<'a> {
    pub fn new(stdout: &'a mut dyn Write, stderr: &'a mut dyn Write, json: bool) -> Self {
        Self {
            stdout,
            stderr,
            json,
            write_error: None,
        }
    }

    /// Whether the caller asked for JSON.
    pub fn json(&self) -> bool {
        self.json
    }

    /// The command's result: JSON on stdout in both modes. This is the one thing `run`, `script`
    /// and `inspect` print, so their output is parseable without `--json`.
    pub fn data(&mut self, value: &Value) {
        let text = serde_json::to_string_pretty(value)
            .unwrap_or_else(|e| format!("{{\"unserializable\": \"{e}\"}}"));
        write(
            self.stdout,
            format!("{text}\n").as_bytes(),
            &mut self.write_error,
        );
    }

    /// A human line on stdout, dropped in `--json` mode so stdout stays JSON only.
    pub fn text(&mut self, line: &str) {
        if self.json {
            return;
        }
        write(
            self.stdout,
            format!("{line}\n").as_bytes(),
            &mut self.write_error,
        );
    }

    /// A diagnostic on stderr, in both modes.
    pub fn note(&mut self, message: &str) {
        write(
            self.stderr,
            format!("note: {message}\n").as_bytes(),
            &mut self.write_error,
        );
    }

    /// Fail a write to stdout that was neither a closed pipe nor a success.
    pub fn check(&self) -> Result<(), CliError> {
        match &self.write_error {
            Some(e) => Err(CliError::Failed(format!("couldn't write the result: {e}"))),
            None => Ok(()),
        }
    }
}

/// Write `bytes`, remembering the first real failure. A closed pipe is what `… | head` looks
/// like, not an error.
fn write(w: &mut dyn Write, bytes: &[u8], error: &mut Option<String>) {
    if let Err(e) = w.write_all(bytes) {
        if e.kind() != io::ErrorKind::BrokenPipe && error.is_none() {
            *error = Some(e.to_string());
        }
    }
}

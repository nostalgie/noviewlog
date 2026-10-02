use chrono::Utc;

use crate::ansi::strip_ansi;
use crate::types::{LogFormat, LogRecord};

/// Maximum lines accumulated in one pending record before it is force-flushed.
/// A stream whose start regex fires once and whose every following line
/// matches a continuation regex (e.g. `^\s+`) would otherwise grow
/// `pending_lines` without bound — `RecordBuffer::max_records` never applies
/// because no record is created (issue #189).
const MAX_PENDING_LINES: usize = 256;

pub struct RecordParser {
    format: LogFormat,
    pending_lines: Vec<String>,
    /// Cached `strip_ansi` per pending line (issue #54): computed once at
    /// push time for start/continuation classification, reused by `flush`.
    /// `None` after `replace_last_pending_line` (recomputed lazily).
    pending_plain: Vec<Option<String>>,
    next_id: u64,
    /// Shared stamp for every record flushed in the current ingest chunk.
    chunk_received_at: chrono::DateTime<Utc>,
}

impl RecordParser {
    pub fn new(format: LogFormat) -> Self {
        Self {
            format,
            pending_lines: Vec::new(),
            pending_plain: Vec::new(),
            next_id: 1,
            chunk_received_at: Utc::now(),
        }
    }

    /// Call once per PTY ingest chunk so flushed records share one timestamp.
    pub fn begin_chunk(&mut self) {
        self.chunk_received_at = Utc::now();
    }

    pub fn set_format(&mut self, format: LogFormat) {
        self.format = format;
    }

    pub fn push_line(&mut self, line: String) -> Vec<LogRecord> {
        let mut records = Vec::new();
        let plain = strip_ansi(&line);

        if self.is_start_line(&plain) {
            if !self.pending_lines.is_empty() {
                records.push(self.flush());
            }
            self.pending_lines = vec![line];
            self.pending_plain = vec![Some(plain)];
            return records;
        }

        if !self.pending_lines.is_empty() && self.is_continuation(&plain) {
            // Force-flush a full pending record so a continuation-only
            // stream cannot accumulate without bound (issue #189).
            if self.pending_lines.len() >= MAX_PENDING_LINES {
                records.push(self.flush());
            }
            self.pending_lines.push(line);
            self.pending_plain.push(Some(plain));
            return records;
        }

        if !self.pending_lines.is_empty() {
            records.push(self.flush());
        }

        self.pending_lines = vec![line];
        self.pending_plain = vec![Some(plain)];
        records
    }

    pub fn flush_pending(&mut self) -> Option<LogRecord> {
        if self.pending_lines.is_empty() {
            None
        } else {
            Some(self.flush())
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending_lines.is_empty()
    }

    /// Replace the in-progress pending line (spinner frame) without flushing to the buffer.
    pub fn replace_last_pending_line(&mut self, line: String) {
        if let Some(last) = self.pending_lines.last_mut() {
            *last = line;
            let idx = self.pending_lines.len() - 1;
            if let Some(slot) = self.pending_plain.get_mut(idx) {
                *slot = None;
            }
        } else {
            self.pending_lines.push(line);
            self.pending_plain.push(None);
        }
    }

    fn flush(&mut self) -> LogRecord {
        let lines = std::mem::take(&mut self.pending_lines);
        let plains = std::mem::take(&mut self.pending_plain);
        let text = lines
            .iter()
            .zip(plains)
            .map(|(l, cached)| cached.unwrap_or_else(|| strip_ansi(l)))
            .collect::<Vec<_>>()
            .join("\n");
        let record = LogRecord {
            id: self.next_id,
            lines,
            text,
            received_at: self.chunk_received_at,
            level: None,
            overwrite: false,
        };
        self.next_id += 1;
        record
    }

    fn is_start_line(&self, plain: &str) -> bool {
        if self.format.id == "raw" {
            return true;
        }
        self.format
            .start_regex
            .as_ref()
            .is_some_and(|re| re.is_match(plain))
    }

    fn is_continuation(&self, plain: &str) -> bool {
        if self.format.continuation_regexes.is_empty() {
            return false;
        }
        self.format
            .continuation_regexes
            .iter()
            .any(|re| re.is_match(plain))
    }
}

pub fn reparse_lines(lines: &[String], format: LogFormat) -> Vec<LogRecord> {
    let mut parser = RecordParser::new(format);
    let mut records = Vec::new();
    for line in lines {
        records.extend(parser.push_line(line.clone()));
    }
    if let Some(last) = parser.flush_pending() {
        records.push(last);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn continuation_only_stream_is_bounded() {
        // Issue #189: after one start line, lines that all match a
        // continuation regex must not accumulate without bound.
        let format = LogFormat {
            id: "python-test".into(),
            name: "python-test".into(),
            start: r"^\[start\]".into(),
            continuation: vec![r"^\s+".into()],
            start_regex: Some(Arc::new(regex::Regex::new(r"^\[start\]").unwrap())),
            continuation_regexes: vec![
                Arc::new(regex::Regex::new(r"^\s+").unwrap()),
                Arc::new(regex::Regex::new(r"^\s*$").unwrap()),
            ],
        };
        let mut parser = RecordParser::new(format);
        parser.push_line("[start] trace".to_string());
        let mut produced = 0usize;
        for i in 0..10_000 {
            let recs = parser.push_line(format!("  line {i}"));
            produced += recs.len();
        }
        // Pending flushes every MAX_PENDING_LINES continuation lines instead
        // of growing without bound.
        assert_eq!(produced, 10_000 / MAX_PENDING_LINES);
        let recs = parser.push_line("[start] next".to_string());
        assert_eq!(
            recs.len(),
            1,
            "pending record flushes on the next start line"
        );
        assert!(parser.has_pending());
    }
}

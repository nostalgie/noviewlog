use std::collections::VecDeque;

use crate::types::LogRecord;

pub struct RecordBuffer {
    records: VecDeque<LogRecord>,
    dropped: usize,
    max_records: usize,
}

impl RecordBuffer {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: VecDeque::new(),
            dropped: 0,
            max_records,
        }
    }

    /// Drop oldest records when over `max_records`.
    /// Returns `(records removed, raw lines removed)` from the front.
    ///
    /// Uses `pop_front` so cost is O(dropped), not O(capacity).
    fn trim_overflow(&mut self) -> (usize, usize) {
        if self.records.len() <= self.max_records {
            return (0, 0);
        }
        let overflow = self.records.len() - self.max_records;
        let mut shifted_records = 0usize;
        let mut shifted_lines = 0usize;
        for _ in 0..overflow {
            let Some(rec) = self.records.pop_front() else {
                break;
            };
            shifted_lines += rec.lines.len();
            shifted_records += 1;
            self.dropped += 1;
        }
        (shifted_records, shifted_lines)
    }

    /// Returns the number of raw lines dropped from the front when the cap is exceeded.
    pub fn add(&mut self, record: LogRecord) -> usize {
        self.add_counting(record).1
    }

    /// Like [`Self::add`], but also returns how many records were dropped.
    /// Multiline records make the two counts differ; ring-shift patching needs
    /// both (flat lines for the flat-line prefix, records for the record cursor).
    pub fn add_counting(&mut self, record: LogRecord) -> (usize, usize) {
        self.records.push_back(record);
        // Compact while the ring is already hot (issue #126): after this, the
        // deque stays contiguous until the next push actually wraps, so the
        // several `records()` callers per tick stop paying O(ring) churn each.
        self.records.make_contiguous();
        self.trim_overflow()
    }

    /// Contiguous record slice (compacts the ring if needed).
    pub fn records(&mut self) -> &[LogRecord] {
        self.records.make_contiguous()
    }

    /// All physical lines from records (cloned). Used for format reparse.
    pub fn raw_lines(&self) -> Vec<String> {
        self.records
            .iter()
            .flat_map(|r| r.lines.iter().cloned())
            .collect()
    }

    pub fn records_len(&self) -> usize {
        self.records.len()
    }

    pub fn raw_lines_len(&self) -> usize {
        self.records.iter().map(|r| r.lines.len()).sum()
    }

    pub fn dropped_count(&self) -> usize {
        self.dropped
    }

    pub fn max_records(&self) -> usize {
        self.max_records
    }

    /// Update the retention cap and drop oldest records if over the new limit.
    /// Returns the number of raw lines removed from the front.
    pub fn set_max_records(&mut self, max_records: usize) -> usize {
        self.max_records = max_records.max(1);
        self.trim_overflow().1
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.dropped = 0;
    }

    /// Replace all records (file window swap). Resets dropped count.
    pub fn replace_all(&mut self, records: Vec<LogRecord>) {
        self.records.clear();
        self.dropped = 0;
        for record in records {
            self.records.push_back(record);
        }
        self.trim_overflow();
    }

    pub fn last_is_overwrite_single_line(&self) -> bool {
        self.records
            .back()
            .is_some_and(|r| r.lines.len() == 1 && r.overwrite)
    }

    pub fn set_last_overwrite(&mut self, overwrite: bool) {
        if let Some(last) = self.records.back_mut() {
            last.overwrite = overwrite;
        }
    }

    /// Replace the last single-line overwrite record (spinner / progress).
    /// Returns true if a record was replaced.
    pub fn replace_last_single_line(&mut self, record: LogRecord) -> bool {
        if self.last_is_overwrite_single_line() {
            let _ = self.records.pop_back();
            self.add(record);
            return true;
        }
        self.add(record);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLevel;
    use chrono::Utc;

    fn rec(id: u64, text: &str) -> LogRecord {
        LogRecord {
            id,
            lines: vec![text.to_string()],
            text: text.to_string(),
            received_at: Utc::now(),
            level: None::<LogLevel>,
            overwrite: false,
        }
    }

    #[test]
    fn add_past_max_records_drops_oldest() {
        let mut buf = RecordBuffer::new(3);
        assert_eq!(buf.add(rec(1, "a")), 0);
        assert_eq!(buf.add(rec(2, "b")), 0);
        assert_eq!(buf.add(rec(3, "c")), 0);
        assert_eq!(buf.records_len(), 3);
        assert_eq!(buf.dropped_count(), 0);

        let shifted = buf.add(rec(4, "d"));
        assert_eq!(shifted, 1);
        assert_eq!(buf.records_len(), 3);
        assert_eq!(buf.dropped_count(), 1);
        assert_eq!(
            buf.records().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(
            buf.raw_lines(),
            vec!["b".to_string(), "c".to_string(), "d".to_string()]
        );
    }

    #[test]
    fn trim_many_at_cap_is_linear_in_overflow() {
        // Smoke: filling far past cap must not hang (old Vec::drain was O(cap) per line).
        let mut buf = RecordBuffer::new(1_000);
        for i in 0..50_000u64 {
            buf.add(rec(i, "x"));
        }
        assert_eq!(buf.records_len(), 1_000);
        assert_eq!(buf.dropped_count(), 49_000);
    }
}

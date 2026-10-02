//! Record ingestion into the active terminal's ring buffer.

use super::*;

impl Engine {
    pub(crate) fn push_lines(&mut self, lines: impl IntoIterator<Item = String>, mark_dirty: bool) {
        let tracking_window = self.has_active_terminal()
            && (self.active_terminal().file_load.is_some()
                || self.active_terminal().file_backed.is_some());
        let mut shifted = false;
        {
            let terminal = self.active_terminal_mut();
            if terminal.buffer.last_is_overwrite_single_line() {
                terminal.buffer.set_last_overwrite(false);
            }
            for line in lines {
                let records = terminal.parser.push_line(line);
                for record in records {
                    let shifted_lines = terminal.buffer.add(record);
                    if tracking_window && shifted_lines > 0 {
                        terminal.buffer_line_start += shifted_lines as u64;
                        shifted = true;
                    }
                }
            }
            terminal.last_line_at = Some(Instant::now());
        }
        if mark_dirty || shifted {
            self.mark_all_views_dirty();
        }
        // Only paint when callers ask (`mark_dirty`) or the window shifted.
        // File load uses mark_dirty=false and paints explicitly at first/last batch.
        if mark_dirty || shifted {
            self.mark_viewport_dirty();
        }
    }

    pub(crate) fn flush_idle_pending(&mut self) {
        // Every terminal, not just the active one (issue #193): a background
        // terminal's last line stays pending in its RecordParser and must not
        // wait for the user to switch back to it.
        for term_idx in 0..self.terminals.len() {
            let should_flush = self.terminals[term_idx]
                .last_line_at
                .is_some_and(|at| at.elapsed() >= PENDING_IDLE_FLUSH);
            if !should_flush {
                continue;
            }
            let flushed = {
                let terminal = &mut self.terminals[term_idx];
                terminal
                    .ingest
                    .idle_flush(&mut terminal.buffer, &mut terminal.parser)
            };
            self.terminals[term_idx].last_line_at = None;
            if !flushed {
                continue;
            }
            if term_idx == self.active_terminal {
                self.mark_all_views_dirty();
            } else {
                for view in &mut self.terminals[term_idx].views {
                    view.mark_flat_lines_dirty();
                }
            }
        }
    }
}

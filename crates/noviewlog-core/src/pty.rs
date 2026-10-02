use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::spawn_resolve::PreparedSpawn;

/// Optional host wake after PTY I/O is posted (Slint event loop, etc.).
pub type PtyActivityWake = Arc<dyn Fn() + Send + Sync>;

/// Raw PTY output. Terminal emulation (cursor/erase/scrollback) happens in the
/// consumer via [`crate::core::terminal::TerminalIngest`], not here — the read
/// loop only frames raw bytes so nothing is lost to premature line splitting.
#[derive(Debug)]
pub enum PtyEvent {
    /// A raw chunk of PTY output bytes for a specific terminal session.
    /// `generation` matches the [`PtyManager::start_prepared`] call that
    /// spawned the reader — leftover `Bytes` from a stopped / restarted
    /// session are dropped by the consumer instead of polluting the new
    /// session.
    Bytes {
        id: String,
        data: Vec<u8>,
        generation: u64,
    },
    /// Child exited. `generation` matches the [`PtyManager::start_prepared`]
    /// call that spawned this child — leftover `Exit` from a previous session
    /// must be ignored.
    Exit {
        id: String,
        code: i32,
        generation: u64,
    },
}

/// Master handle plus the [`PtyManager::start_prepared`] sequence token that
/// created it. The seq lets a stale waiter drop only **its own** master —
/// never the master of a newer session started on the same manager.
struct MasterSlot {
    seq: u64,
    master: Box<dyn MasterPty + Send>,
}

/// Bounded stdin queue depth (issue #58). `write_bytes` uses `try_send`, so a
/// child that never reads stdin fills the queue and the write fails fast with
/// a status message instead of blocking the UI thread in `write_all`.
const STDIN_QUEUE_CAPACITY: usize = 256;

/// Poll interval for the reader's full-queue send retry (issue #196). A
/// blocking `SyncSender::send` parks until the queue drains or the receiver
/// drops — `stop()` (kill child, drop master) can unblock `read`, but never
/// `send`, so the reader and the exit-waiter joined to it leaked until the
/// engine dropped `pty_rx`. With `try_send` + this interval, `stop()` breaks
/// the wait within one tick; bursts still fill the queue without sleeping,
/// so the cap only applies while the consumer is the bottleneck
/// (~4 KB chunks → ≥40 MB/s sustained feed).
const QUEUE_FULL_RETRY: Duration = Duration::from_micros(100);

/// Session-stop-aware queue send (issue #196): retries [`QUEUE_FULL_RETRY`]
/// while the bounded queue is full, re-checking `running` every attempt so
/// [`PtyManager::stop`] releases a reader parked on a flooded queue. Returns
/// false — dropping `event` — when the session was stopped or the channel
/// disconnected; bytes of a stopped session are stale by definition.
fn send_until_accepted(
    tx: &SyncSender<PtyEvent>,
    running: &AtomicBool,
    mut event: PtyEvent,
) -> bool {
    loop {
        if !running.load(Ordering::SeqCst) {
            return false;
        }
        match tx.try_send(event) {
            Ok(()) => return true,
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                event = returned;
                thread::sleep(QUEUE_FULL_RETRY);
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                running.store(false, Ordering::SeqCst);
                return false;
            }
        }
    }
}

pub struct PtyManager {
    /// Running flag of the CURRENT session. `start_prepared()` replaces the
    /// Arc so a stale exit-waiter of a prior session stores false on a dead
    /// flag and can never silence the next session's reader (issue #106).
    running: Arc<AtomicBool>,
    /// Set by [`Self::stop`]; the exit-waiter thread suppresses its `Exit`
    /// event so an explicit Stop neither respawns a plain shell nor overwrites
    /// the "Stopped" status / exit code.
    stop_requested: Arc<AtomicBool>,
    child_killer: Option<Box<dyn portable_pty::ChildKiller + Send>>,
    /// Bounded stdin queue feeding the dedicated writer thread (`None` when
    /// not running / stopped). UI threads never touch the pty master directly.
    stdin_tx: Option<SyncSender<Vec<u8>>>,
    /// Writer-thread errors surfaced by [`Self::take_stdin_error`].
    stdin_error: Arc<Mutex<Option<String>>>,
    /// True once [`Self::take_stdin_error`] reported the error as a status
    /// event. The error itself stays sticky (until the session restarts) so
    /// `write_bytes` still refuses input after the per-tick drain consumed
    /// the report — otherwise keystrokes would silently queue into a dead
    /// writer channel (#194).
    stdin_error_reported: AtomicBool,
    /// Must outlive the child on Windows: dropping the ConPTY master calls
    /// `ClosePseudoConsole`, which makes the child exit with `0xC0000142`
    /// (`STATUS_DLL_INIT_FAILED`) if it has not finished console init yet.
    /// Shared with the exit-waiter thread: after a **natural** child exit the
    /// waiter drops the master, conhost releases the pipe write end, and the
    /// reader unblocks with EOF (on Windows conhost holds the write end until
    /// `ClosePseudoConsole`, so the reader would otherwise block forever).
    /// Dropping only after full child exit keeps the DLL-init workaround intact.
    master: Arc<Mutex<Option<MasterSlot>>>,
    /// Last size applied to the live PTY (or the size used for the next open).
    size: PtySize,
    /// Session token from the last successful [`Self::start_prepared`]
    /// (0 = never started).
    generation: u64,
    /// Monotonic token for the current master slot (guards stale waiters).
    start_seq: u64,
    /// Exit flags of the CURRENT session's reader + exit-waiter threads,
    /// replaced by every [`Self::start_prepared`]. Leak observability for
    /// tests (issue #196): a flag flips when its thread has exited.
    session_threads_done: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            child_killer: None,
            stdin_tx: None,
            stdin_error: Arc::new(Mutex::new(None)),
            stdin_error_reported: AtomicBool::new(false),
            master: Arc::new(Mutex::new(None)),
            size: PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            },
            generation: 0,
            start_seq: 0,
            session_threads_done: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn lock_master(&self) -> std::sync::MutexGuard<'_, Option<MasterSlot>> {
        self.master.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Remember geometry for the next `start`, and resize a live PTY if any.
    pub fn set_size(&mut self, size: PtySize) -> Result<(), String> {
        let size = PtySize {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        };
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        let slot = self.lock_master();
        if let Some(entry) = slot.as_ref() {
            entry.master.resize(size).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Start an already-resolved process in this PTY. Stops any previous child
    /// of **this** manager only.
    ///
    /// The engine resolves argv via the background [`crate::spawn_resolver`]
    /// (issue #59) and hands over the finished [`PreparedSpawn`] — this method
    /// must never probe PATH / the registry itself.
    pub fn start_prepared(
        &mut self,
        tx: SyncSender<PtyEvent>,
        id: String,
        prepared: PreparedSpawn,
        generation: u64,
        activity_wake: Option<PtyActivityWake>,
    ) -> Result<(), String> {
        self.stop();
        self.generation = generation;
        self.start_seq = self.start_seq.wrapping_add(1);
        let start_seq = self.start_seq;

        // Per-session running flag (issue #106): every thread spawned for this
        // session captures THIS Arc, so a stale exit-waiter of a previous
        // session can never clear the next session's flag (its store lands on
        // the dead Arc).
        let running = Arc::new(AtomicBool::new(false));
        self.running = running.clone();

        // `prepare_spawn` already finalized cwd (never UNC on Windows) and argv.
        let command = prepared.command;
        let args = prepared.args;
        let workdir = prepared.cwd;

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(self.size).map_err(|e| e.to_string())?;

        let mut cmd = CommandBuilder::new(&command);
        cmd.args(&args);
        cmd.cwd(&workdir);
        cmd.env("FORCE_COLOR", "1");
        // Advertise a real terminal so ora/listr2/ink use cursor-addressed
        // progress rendering (which the emulator handles) consistently.
        cmd.env("TERM", "xterm-256color");
        // Some tools read COLUMNS/LINES instead of TIOCGWINSZ.
        cmd.env("COLUMNS", self.size.cols.to_string());
        cmd.env("LINES", self.size.rows.to_string());

        let mut child = pair.slave.spawn_command(cmd).map_err(|e| {
            format!("failed to spawn '{command}' (args: {args:?}) in cwd '{workdir}': {e}")
        })?;
        // Release slave handles only — keep master until stop()/Drop.
        drop(pair.slave);

        // Reader/writer setup can still fail after a successful spawn (issue
        // #111): kill and reap the child instead of orphaning it.
        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        };
        *self.lock_master() = Some(MasterSlot {
            seq: start_seq,
            master: pair.master,
        });

        // Dedicated stdin writer thread (issue #58): the UI only queues into a
        // bounded channel, so a child that never reads stdin applies
        // backpressure here instead of blocking `write_all` in a UI tick.
        let (stdin_tx, stdin_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(STDIN_QUEUE_CAPACITY);
        self.stdin_tx = Some(stdin_tx);
        self.stdin_error = Arc::new(Mutex::new(None));
        self.stdin_error_reported = AtomicBool::new(false);
        let stdin_error = self.stdin_error.clone();
        let running_writer = running.clone();
        thread::spawn(move || {
            let mut writer = writer;
            while let Ok(data) = stdin_rx.recv() {
                if let Err(err) = writer.write_all(&data).and_then(|_| writer.flush()) {
                    *stdin_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
                    break;
                }
                if !running_writer.load(Ordering::SeqCst) {
                    break;
                }
            }
        });

        let killer = child.clone_killer();
        self.child_killer = Some(killer);
        self.stop_requested.store(false, Ordering::SeqCst);
        running.store(true, Ordering::SeqCst);

        let reader_id = id.clone();
        let session_id = id;
        let session_generation = generation;

        // Reader thread: forward raw output until EOF/err. It deliberately does
        // NOT wait for the child or send `Exit` — on Windows ConPTY the reader
        // would block forever after child exit because conhost holds the pipe
        // write end until the master is dropped (and the master must outlive
        // the child for the DLL-init workaround).
        let tx_waiter = tx.clone();
        let reader_wake = activity_wake.clone();
        let bytes_generation = generation;
        let running_reader = running.clone();
        // Leak observability for tests (issue #196): done-flags of the current
        // session's reader + exit-waiter, replaced on every start.
        let reader_done = Arc::new(AtomicBool::new(false));
        let waiter_done = Arc::new(AtomicBool::new(false));
        *self
            .session_threads_done
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = vec![reader_done.clone(), waiter_done.clone()];
        let reader_handle = thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            let session_id = reader_id;
            let activity_wake = reader_wake;
            let running = running_reader;
            let wake = || {
                if let Some(w) = &activity_wake {
                    w();
                }
            };

            loop {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let event = PtyEvent::Bytes {
                            id: session_id.clone(),
                            data: chunk[..n].to_vec(),
                            generation: bytes_generation,
                        };
                        if !send_until_accepted(&tx, &running, event) {
                            break;
                        }
                        wake();
                    }
                    Err(_) => break,
                }
            }
            reader_done.store(true, Ordering::SeqCst);
        });

        // Exit-waiter thread: detects child exit independently of reader EOF.
        let stop_requested = self.stop_requested.clone();
        let master_slot = self.master.clone();
        let tx = tx_waiter;
        let waiter_handle = thread::spawn(move || {
            // portable-pty reports the raw DWORD; cast to i32 so NTSTATUS values
            // like 0xC0000142 surface as the familiar negative -1073741502 in the UI.
            // A wait() failure is NOT a natural exit (issue #111): report the
            // sentinel 258 (0x102, still-active bit) so the UI does not show a
            // normal-looking "exited (1)".
            let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(258);
            // Full child exit: dropping the master now calls ClosePseudoConsole,
            // conhost exits and releases the pipe write end, which unblocks the
            // reader. Dropping strictly after `wait` keeps the 0xC0000142
            // (STATUS_DLL_INIT_FAILED) early-drop workaround intact. The seq
            // guard stops a stale waiter from dropping a newer session's master.
            {
                let mut slot = master_slot.lock().unwrap_or_else(|e| e.into_inner());
                if slot.as_ref().is_some_and(|s| s.seq == start_seq) {
                    *slot = None;
                }
            }
            // Join the reader so every trailing `Bytes` is queued before `Exit`
            // — `ingest.finish()` in poll_pty must not cut off tail output.
            let _ = reader_handle.join();
            running.store(false, Ordering::SeqCst);
            if !stop_requested.load(Ordering::SeqCst) {
                let _ = tx.send(PtyEvent::Exit {
                    id: session_id,
                    code,
                    generation: session_generation,
                });
                if let Some(w) = &activity_wake {
                    w();
                }
            }
            waiter_done.store(true, Ordering::SeqCst);
        });
        // Detached spawn, as before — the waiter is only joined by itself.
        drop(waiter_handle);

        Ok(())
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Suppress the waiter's late `Exit`: an explicit Stop must neither
        // respawn a plain shell nor overwrite the "Stopped" status/exit code.
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(mut killer) = self.child_killer.take() {
            let _ = killer.kill();
        }
        // Drop master last so ConPTY stays valid until the child is signalled.
        *self.lock_master() = None;
        self.stdin_tx = None; // writer thread exits when its queue drains/closes
    }

    /// Send raw bytes to the child process stdin (PTY). Non-blocking (issue
    /// #58): the payload is queued for the writer thread; a full queue or a
    /// dead writer fails fast with an error the UI can show as status.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        let Some(tx) = &self.stdin_tx else {
            return Err("process is not running".to_string());
        };
        // A writer-thread failure (e.g. broken pipe after the child exited)
        // is sticky until the session restarts: even when the engine tick
        // already consumed the status report, later input must fail fast
        // instead of silently queueing into a dead channel (#194).
        if self.has_stdin_error() {
            self.stdin_tx = None;
            return Err("stdin write failed".to_string());
        }
        tx.try_send(data.to_vec()).map_err(|e| match e {
            std::sync::mpsc::TrySendError::Full(_) => {
                "stdin backlog full (child is not reading input)".to_string()
            }
            std::sync::mpsc::TrySendError::Disconnected(_) => "process is not running".to_string(),
        })
    }

    /// Take the last stdin writer error once (polled by the engine tick to
    /// surface it as a status event). The error stays present for
    /// [`Self::has_stdin_error`] so input keeps failing fast until restart.
    pub fn take_stdin_error(&self) -> Option<String> {
        let guard = self.stdin_error.lock().unwrap_or_else(|e| e.into_inner());
        let err = guard.as_ref()?;
        if !self.stdin_error_reported.swap(true, Ordering::Relaxed) {
            Some(err.clone())
        } else {
            None
        }
    }

    /// Sticky stdin-error indicator (not consumed by taking).
    fn has_stdin_error(&self) -> bool {
        self.stdin_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Test-only leak assertion (issue #196): true when every thread of the
    /// CURRENT session has exited. Only meaningful after [`Self::start_prepared`].
    #[cfg(test)]
    pub(crate) fn test_session_threads_finished(&self) -> bool {
        self.session_threads_done
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .all(|f| f.load(Ordering::SeqCst))
    }
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PtyManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Test helper: run raw bytes (in the given chunks) through the terminal
/// emulator and return the final flat log lines (ANSI-stripped).
#[cfg(test)]
pub fn emulate_chunks_to_lines(chunks: &[&[u8]]) -> Vec<String> {
    use crate::core::ansi::strip_ansi;
    use crate::core::buffer::RecordBuffer;
    use crate::core::formats::get_builtin_format;
    use crate::core::parser::RecordParser;
    use crate::core::terminal::TerminalIngest;

    let mut ingest = TerminalIngest::new();
    let mut buffer = RecordBuffer::new(100_000);
    let mut parser = RecordParser::new(get_builtin_format("node-default"));
    for chunk in chunks {
        ingest.feed(chunk, &mut buffer, &mut parser);
    }
    ingest.finish(&mut buffer, &mut parser);

    buffer
        .records()
        .iter()
        .flat_map(|r| r.lines.iter())
        .map(|l| strip_ansi(l))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_glued_spinner(lines: &[String]) -> bool {
        lines.iter().any(|s| {
            let spinner = s
                .chars()
                .any(|c| matches!(c, '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏'));
            spinner && s.contains('✔')
        })
    }

    fn assert_strapi_startup(lines: &[String], ctx: &str) {
        assert!(
            !has_glued_spinner(lines),
            "[{ctx}] glued spinner+check line: {lines:?}"
        );
        for needle in [
            "✔ Cleaning dist dir",
            "✔ Compiling TS",
            "Project information",
            "Actions available",
            "Welcome back!",
            "http://localhost:1337",
        ] {
            assert!(
                lines.iter().any(|l| l.contains(needle)),
                "[{ctx}] missing {needle:?} in {lines:?}"
            );
        }
        let table_rows = lines
            .iter()
            .filter(|l| l.contains('│') || l.contains('╭') || l.contains('╰'))
            .count();
        assert!(table_rows >= 9, "[{ctx}] too few table rows: {table_rows}");
        // Every ✔ step must survive as its own committed line, not collapse away.
        for step in ["Cleaning dist dir", "Compiling TS"] {
            assert_eq!(
                lines
                    .iter()
                    .filter(|l| l.contains('✔') && l.contains(step))
                    .count(),
                lines
                    .iter()
                    .filter(|l| l.contains(step) && l.contains('✔'))
                    .count(),
                "[{ctx}] {step}"
            );
        }
    }

    #[test]
    fn interactive_capture_all_chunk_sizes() {
        let bytes = include_bytes!("../test_data/strapi-portable-pty.bin");
        for size in [1usize, 7, 37, 64, 512, 4096] {
            let chunks: Vec<&[u8]> = bytes.chunks(size).collect();
            let lines = emulate_chunks_to_lines(&chunks);
            assert_strapi_startup(&lines, &format!("interactive chunk={size}"));
        }
    }

    #[test]
    fn noninteractive_capture_preserves_steps() {
        let bytes = include_bytes!("../test_data/strapi-develop-sample.bin");
        let chunks: Vec<&[u8]> = bytes.chunks(64).collect();
        let lines = emulate_chunks_to_lines(&chunks);
        assert!(!has_glued_spinner(&lines));
        assert!(lines.iter().any(|l| l.contains("Loading Strapi")));
        assert!(lines.iter().any(|l| l.contains("Compiling TS")));
    }

    #[test]
    fn utf8_box_drawing_survives_chunk_splits() {
        let border = "╭────┬────╮\r\n";
        let bytes = border.as_bytes();
        for size in 1..=bytes.len() {
            let chunks: Vec<&[u8]> = bytes.chunks(size).collect();
            let lines = emulate_chunks_to_lines(&chunks);
            assert!(
                lines.iter().any(|l| l.contains('╭') && l.contains('╮')),
                "chunk={size} lines={lines:?}"
            );
        }
    }

    #[test]
    fn plain_lines_pass_through() {
        let lines = emulate_chunks_to_lines(&[b"hello\r\n", b"world\r\n"]);
        assert!(lines.iter().any(|l| l.contains("hello")));
        assert!(lines.iter().any(|l| l.contains("world")));
    }

    #[test]
    fn spinner_frames_collapse_to_final_check() {
        // CSI cursor-up + redraw: final ✔ should win, spinner frames discarded.
        let frames: &[&[u8]] = &[
            b"\x1b[?25l\x1b[1G\x1b[0K\x1b[32m\xe2\xa0\x8b\x1b[0m Cleaning\r\n",
            b"\x1b[1A\x1b[1G\x1b[0K\x1b[32m\xe2\xa0\x99\x1b[0m Cleaning\r\n",
            b"\x1b[1A\x1b[1G\x1b[0K\x1b[32m\xe2\x9c\x94\x1b[0m Cleaning\r\n",
        ];
        let lines = emulate_chunks_to_lines(frames);
        assert!(!has_glued_spinner(&lines));
        assert!(lines
            .iter()
            .any(|l| l.contains('✔') && l.contains("Cleaning")));
    }

    #[test]
    fn separate_completion_lines_are_all_kept() {
        let lines =
            emulate_chunks_to_lines(&["✔ step one\r\n".as_bytes(), "✔ step two\r\n".as_bytes()]);
        assert_eq!(
            lines.iter().filter(|l| l.contains('✔')).count(),
            2,
            "{lines:?}"
        );
    }

    #[test]
    fn stdin_error_stays_sticky_after_status_report() {
        // Issue #194: the per-tick drain (surface_stdin_errors) takes the
        // error for the status event; write_bytes must still fail fast
        // instead of silently queueing keystrokes into the dead writer.
        let mut pty = PtyManager::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        pty.stdin_tx = Some(tx);
        *pty.stdin_error.lock().unwrap() = Some("broken pipe".to_string());

        assert_eq!(
            pty.take_stdin_error().as_deref(),
            Some("broken pipe"),
            "first tick surfaces the error"
        );
        assert_eq!(pty.take_stdin_error(), None, "reported only once");

        assert!(
            pty.write_bytes(b"x").is_err(),
            "input must fail fast while the error is sticky"
        );
        assert!(pty.stdin_tx.is_none(), "dead writer channel dropped");
    }
}

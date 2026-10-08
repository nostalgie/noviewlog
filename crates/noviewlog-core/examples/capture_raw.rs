// Capture RAW pty bytes from a command, matching PtyManager settings exactly
// (FORCE_COLOR, TERM, COLUMNS, LINES).
// Usage: capture_raw <out_file> <seconds> -- <command> [args...]
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = &args[1];
    let secs: u64 = args[2].parse().unwrap();
    let sep = args.iter().position(|a| a == "--").unwrap();
    let cmd_parts = &args[sep + 1..];
    let cwd = std::env::var("CAP_CWD").ok();

    let pty_system = native_pty_system();
    let size = PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system.openpty(size).unwrap();
    let mut cmd = CommandBuilder::new(&cmd_parts[0]);
    cmd.args(&cmd_parts[1..]);
    if let Some(c) = cwd {
        cmd.cwd(c);
    }
    cmd.env("FORCE_COLOR", "1");
    cmd.env("TERM", "xterm-256color");
    // Some tools read COLUMNS/LINES instead of TIOCGWINSZ (same as PtyManager).
    cmd.env("COLUMNS", size.cols.to_string());
    cmd.env("LINES", size.rows.to_string());
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();

    // Kill the child after `secs` so the blocking read unblocks on EOF.
    let mut killer = child.clone_killer();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        let _ = killer.kill();
    });

    let mut file = std::fs::File::create(out).unwrap();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                file.write_all(&chunk[..n]).unwrap();
                file.flush().unwrap();
            }
            Err(_) => break,
        }
    }
    let _ = child.wait();
    eprintln!("captured to {out}");
}

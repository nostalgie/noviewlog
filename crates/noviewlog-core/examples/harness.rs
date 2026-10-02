// Dump final flat log lines produced by the terminal emulator for a capture.
// Usage: harness <file.bin> [chunk_size]
use noviewlog_core::core::ansi::strip_ansi;
use noviewlog_core::core::buffer::RecordBuffer;
use noviewlog_core::core::formats::get_builtin_format;
use noviewlog_core::core::parser::RecordParser;
use noviewlog_core::core::terminal::TerminalIngest;

fn emulate(bytes: &[u8], chunk_size: usize) -> Vec<String> {
    let mut ingest = TerminalIngest::new();
    let mut buffer = RecordBuffer::new(100_000);
    let mut parser = RecordParser::new(get_builtin_format("node-default"));
    for chunk in bytes.chunks(chunk_size) {
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let chunk_size: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4096);
    let bytes = std::fs::read(path).unwrap();
    let lines = emulate(&bytes, chunk_size);
    println!(
        "### {path}  chunk_size={chunk_size}  ({} lines)\n",
        lines.len()
    );
    for (i, l) in lines.iter().enumerate() {
        println!("{i:3} | {l}");
    }
}

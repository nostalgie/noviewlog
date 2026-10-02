# noviewlog-terminal

NoViewLog terminal layer — the GUI-independent half of the engine.

Turns a raw byte stream into filterable, styled log records:

- `terminal` — VTE screen emulation (grid, scrollback, wide-char handling)
- `parser` — record grouping (start/continuation regexes per log format)
- `buffer` — bounded record ring with compaction
- `filter` — include/exclude filters and severity filtering
- `ansi` — SGR parsing, escape stripping, search-highlight overlay
- `visible` — projection of records to displayable styled lines

No PTY, no fonts, no filesystem, no GUI: a host supplies bytes and consumes
records / `FlatLine`s. Product concerns (config files, presets, engine
façade, viewport) live in `noviewlog-core`.

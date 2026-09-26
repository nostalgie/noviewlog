# NoViewLog

NoViewLog is a native desktop live log workspace for developers who run builds,
dev servers, and other processes locally and need to actually read their output:
launch a command or open a log file, slice it with filter tabs, follow live
output, and search large logs fast.

It is not a log-collection or monitoring tool: it does not aggregate logs from
remote machines, ship them anywhere, or alert.

**Status:** active development. **Linux** and **Windows** are equally
supported; other OSes are best-effort.

## Features

### Sessions

- Launch a command or open a log file (from the app, or from the command line
  with `--file` / `-f`)
- Run multiple independent terminals under **TERMINALS**; log files under
  **FILES** — switching the viewport does not stop other live sessions
- Type into a live process on the Terminal tab; copy and paste (including
  middle-click paste)
- Open a log in a dedicated view-only session; reopening the same path
  switches to it and reloads
- Sidebar: add or close sessions, rename **TERMINALS** rows, Start/Stop or
  Refresh, drag-reorder; the working directory is tracked automatically

### Projects

- **File → Projects…** — create, open, rename, or delete Projects (Programs
  with launch settings + filter tabs)
- Opening a Project (or restoring the last one on startup) restores its
  terminals and files; Programs stay **stopped** until you press Start
- On Windows, a Program's launch can be set to run inside **WSL**

### Tabs and filters

- Each session has a pinned primary tab plus optional filter tabs
- Include and exclude rules (literal or regex): add, toggle, edit, and remove;
  the draft highlights matches while you type
- A per-tab severity mode after include/exclude: All, Errors, Warnings, Info,
  Debug, or Unleveled
- Add, close, restore the last closed tab, rename, and drag-reorder filter
  tabs

### Find and viewport

- Find bar (Ctrl/Cmd+F): case, whole word, and regex; next/previous match and
  match count
- Follow live output; wrap lines or scroll horizontally; smooth scrolling
  through very large files
- Zoom (View menu, Ctrl/Cmd +/−/0, or Ctrl+wheel); the font size is remembered
- Select text (drag, double-click a word, triple-click a record) and copy
- ANSI colors; multiline records such as stack traces are grouped and collapsed
  by default (click to expand, or View → Expand/Collapse all)
- Severity gutter cues on leveled records
- Clickable links emitted by tools (build systems, `git`, `docker build`, …)
  open via the system handler
- Emoji, combining diacritics, and hyperlinks render correctly; fixed monospace
  grid keeps columns aligned

## Download

Grab a build for your platform from the
[Releases](../../releases) page:

- **Windows** — unzip and run `NoViewLog.exe` (no installation required)
- **Linux** — extract the archive and run the binary

## Terminal mode (TUI)

Every release also ships `noviewlog-tui`, a lightweight terminal UI for
watching logs and SSH sessions straight from a terminal — no window needed.

- Runs any local shell in a PTY-backed tab, plus saved SSH profiles
  (`tui_ssh_profiles` in the config file)
- Mouse wheel scrolls output; drag-select text to copy it (right-click for
  copy/filter actions); filters are reversible — clear them with Ctrl+L
- Filter tabs let you narrow the output without touching the original
  stream; fully keyboard-drivable (Ctrl+F, Alt+1..9, Ctrl+Tab, Ctrl+W)
- Static single binary, works inside any terminal, including remote SSH
  sessions

Download `noviewlog-tui` from the release assets and run it in your
terminal (Windows Terminal recommended on Windows).

## Run from source

Requires the Rust toolchain. The run scripts build on first launch, so there is
no separate build step unless you want one.

### Linux

```bash
sudo apt install build-essential pkg-config libssl-dev   # native build tools (Ubuntu)
bash scripts/run-slint.sh
bash scripts/run-slint.sh -- app.log
bash scripts/run-slint.sh -- npm run dev
```

### Windows

Prerequisites: Windows 10+ x64, [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the **Desktop development with C++** workload, and the Rust toolchain
(`rustup-init.exe` from [https://win.rustup.rs/x86_64](https://win.rustup.rs/x86_64),
default host `x86_64-pc-windows-msvc`).

```powershell
.\scripts\run-slint-windows.ps1
.\scripts\run-slint-windows.ps1 -- README.md
.\scripts\run-slint-windows.ps1 -- npm run dev
```

Do **not** use the Linux `run-slint.sh` helper on Windows (and vice versa).

### Command-line flags

- `--preset` / `-p` — apply a filter preset at launch
- `--file` / `-f` — open a log file
- `--config` / `-c` — use a different config file

## Filter logic

1. Exclude rules hide matching records.
2. If any include rule is active, a record must match at least one.
3. Exclude rules take precedence.

Severity is applied after include/exclude.

## Configuration

### File locations

- User config: `~/.config/noviewlog/config.yaml` (Windows:
  `%USERPROFILE%\.config\noviewlog\config.yaml`)
- Projects store: `~/.config/noviewlog/projects.yaml` (Windows:
  `%USERPROFILE%\.config\noviewlog\projects.yaml`)
- Settings: maximum scrollback lines

### Windows shell preference

`shell:` selects the shell used for empty Terminal tabs (Windows only; Unix
uses `$SHELL`):

```yaml
shell: auto        # default: pwsh when installed, else powershell.exe
# shell: pwsh       # PowerShell 7
# shell: powershell # Windows PowerShell 5.1
# shell: cmd        # cmd.exe
```

### Presets

- Bundled filter presets ship with the app (`node-dev`, `node-errors`,
  `php-dev`, `php-errors`, `python-dev`, `python-errors`, `go-errors`,
  `nginx-access`, `docker-compose`)
- Edit or add presets under `presets:` in your user config — the same id
  overrides the bundled definition; new ids are added. Bundled presets you
  omit still load

## License

NoViewLog is licensed under the [MIT License](LICENSE).
Bundled fonts are under the SIL Open Font License (see [`assets/OFL.txt`](assets/OFL.txt)).

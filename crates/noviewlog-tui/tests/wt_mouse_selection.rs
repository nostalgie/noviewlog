//! Real Windows Terminal e2e for TUI mouse selection (slow tier, `#[ignore]`
//! at birth). Run on a Windows dev host with an interactive desktop:
//!
//! ```text
//! cargo build --profile release-dev -p noviewlog-tui
//! cargo test -p noviewlog-tui --test wt_mouse_selection -- --ignored --test-threads=1
//! ```
//!
//! Unlike `conpty_mouse_selection.rs` (which injects SGR bytes and models the
//! conhost VT decoder), this test drives the real Windows Terminal frontend:
//! spawns `wt.exe -w new` running the **release-dev** binary (product path —
//! never debug: debug spawns can stall under PTY flood, see the close-out
//! freeze classification), types a combined shell command via SendKeys, drags
//! the real mouse across part of the sentinel via SendInput, and asserts
//!
//! (a) the clipboard holds the selected text exactly (end-inclusive quirk:
//!     k+1 chars), and
//! (b) the painted highlight via screen pixel sampling: WT's palette blue
//!     (SGR 44 → Campbell #0037DA) covers exactly cells [start, start+k)
//!     and does not stretch toward EOL (the regression fixed in
//!     `render.rs queue_segments`).
//!
//! Geometry anchoring, deliberately WITHOUT UIA text ranges: Windows
//! Terminal's TextPattern range walking proved unreliable on this host
//! (MoveEndpointByUnit on the End endpoint is a no-op, FindText returns
//! E_FAIL, bounding rects of walked ranges are garbage). Instead the typed
//! command
//!
//! ```powershell
//! $b='#'*80
//! write-host $b -b darkyellow -f darkyellow
//! write-host $b -b darkcyan   -f darkcyan
//! echo NVL<pid><millis>
//! ```
//!
//! paints two solid 80-char anchor bars (colors nothing else in the window
//! uses; glyphs invisible because fg == bg) and the sentinel text on the
//! row right below the cyan bar — the echo output always follows the last
//! write-host row, so a wrapped command line above cannot shift it. A
//! LockBits pixel scan of a CopyFromScreen capture of the freshly raised WT
//! window (each scan refocuses the window first, so nothing overlaps it;
//! PrintWindow is unusable — WT renders via DirectX and returns stale
//! frames) finds each bar by color and recovers the content origin, the
//! char cell width (bar width / 80) and the row height — everything the
//! drag and the assertions need.
//!
//! All screen work (SendKeys, SendInput, CopyFromScreen) lives in the
//! embedded PowerShell helper so the test adds NO Rust dependency; the
//! helper thread is forced per-monitor-DPI-v2 so Win32 and GDI coordinates
//! agree in physical pixels.
//!
//! Do not touch the mouse/keyboard while the test runs, and keep the screen
//! unlocked: the freshly opened WT window must stay visible and focused.
//! Teardown kills the spawned `WindowsTerminal.exe` process tree (only PIDs
//! that did not exist before the spawn) and restores the cursor position.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// UIA window lookup + focus, SendKeys, SendInput drag and pixel sampling,
/// one subcommand per invocation. Exits non-zero with a reason on stderr
/// when a step cannot be performed (`no-window`, `no-bar`, ...).
const HELPER_PS: &str = r##"
param(
  [Parameter(Mandatory=$true)][string]$Cmd,
  [int]$ProcId = 0,
  [string]$Needle = '',
  [int]$X1 = 0,
  [int]$Y1 = 0,
  [int]$X2 = 0,
  [int]$Y2 = 0,
  [int]$L = 0,
  [int]$T = 0,
  [int]$W = 0,
  [int]$H = 0,
  [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient | Out-Null
Add-Type -AssemblyName UIAutomationTypes | Out-Null
Add-Type -AssemblyName System.Drawing | Out-Null
Add-Type -AssemblyName System.Windows.Forms | Out-Null

if (-not ('NvlNative' -as [type])) {
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class NvlNative {
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
  public const uint F_MOVE = 1;
  public const uint F_LEFTDOWN = 2;
  public const uint F_LEFTUP = 4;
  public const uint F_ABS = 0x8000;
  public const uint F_VIRT = 0x4000;
  public static INPUT Mouse(int dx, int dy, uint flags) {
    var i = new INPUT();
    i.type = 0;
    i.u.mi = new MOUSEINPUT { dx = dx, dy = dy, dwFlags = flags };
    return i;
  }
  public static void ToAbs(int x, int y, out int nx, out int ny) {
    int vx = GetSystemMetrics(76);
    int vy = GetSystemMetrics(77);
    int vw = GetSystemMetrics(78);
    int vh = GetSystemMetrics(79);
    nx = (int)(((long)(x - vx) * 65535L) / (vw - 1));
    ny = (int)(((long)(y - vy) * 65535L) / (vh - 1));
  }
  public static void SendMouse(INPUT[] ins) {
    SendInput((uint)ins.Length, ins, Marshal.SizeOf(typeof(INPUT)));
  }
  // kind 0 = dark yellow ink (SGR 33 as themed by the TUI, about #D29922,
  // with ClearType blends down to about #A97C1D). kind 1 = dark cyan ink
  // (SGR 36 as themed, about #39C5CF). Both also match the nominal Campbell
  // colors if a future build paints them as fills. Grays and whites never
  // pass.
  //
  // The TUI's log renderer draws shell output as colored glyph ink over the
  // theme background (solid cell backgrounds are reserved for its own
  // overlays, e.g. selection blue), so bars appear as about 20 rows with a
  // large per-row count of matching ink pixels instead of solid fills.
  // Cluster rows with >= 100 matching px (gap <= 2) and return the biggest
  // cluster; a >= 400 px total rejects any single syntax-colored text row.
  // Returns [firstRow, lastRow, minX, maxX, total] or null.
  public static int[] ScanColor(byte[] buf, int stride, int w, int h, int kind) {
    int bFirst = -1, bLast = -1, bMinX = 0, bMaxX = 0, bTotal = 0;
    int cFirst = -1, cLast = -1, cMinX = int.MaxValue, cMaxX = -1, cTotal = 0;
    for (int y = 0; y < h; y++) {
      int rowBase = y * stride; int cnt = 0; int minx = int.MaxValue, maxx = -1;
      for (int x = 0; x < w; x++) {
        int o = rowBase + x * 4;
        int b = buf[o], g = buf[o + 1], r = buf[o + 2];
        bool m = kind == 0 ? (r > 150 && g > 110 && b < 90) : (g > 120 && r < 120 && b > 170);
        if (m) { cnt++; if (x < minx) minx = x; if (x > maxx) maxx = x; }
      }
      if (cnt >= 100) {
        if (cFirst >= 0 && y - cLast <= 2) {
          cLast = y; cTotal += cnt;
          if (minx < cMinX) cMinX = minx; if (maxx > cMaxX) cMaxX = maxx;
        } else {
          if (cTotal > bTotal) { bFirst = cFirst; bLast = cLast; bMinX = cMinX; bMaxX = cMaxX; bTotal = cTotal; }
          cFirst = y; cLast = y; cMinX = minx; cMaxX = maxx; cTotal = cnt;
        }
      }
    }
    if (cTotal > bTotal) { bFirst = cFirst; bLast = cLast; bMinX = cMinX; bMaxX = cMaxX; bTotal = cTotal; }
    if (bLast < 0 || bTotal < 400) return null;
    return new int[] { bFirst, bLast, bMinX, bMaxX, bTotal };
  }
  // Selection blue: SGR 44 as rendered by the WT palette (Campbell
  // #0037DA). Strictly bluer than the cyan anchor: G far below B, and
  // grays/whites never pass. Band = (x0, y0, bw, bh) inside the capture.
  public static int[] ScanBlue(byte[] buf, int stride, int w, int h, int x0, int y0, int bw, int bh) {
    int first = -1, last = -1, count = 0;
    for (int y = y0; y < y0 + bh && y < h; y++) {
      int rowBase = y * stride;
      for (int x = x0; x < x0 + bw && x < w; x++) {
        int o = rowBase + x * 4;
        int b = buf[o], g = buf[o + 1], r = buf[o + 2];
        if ((b - g) > 100 && b > 150 && (b - r) > 60) {
          if (first < 0) first = x - x0;
          last = x - x0;
          count++;
        }
      }
    }
    return new int[] { first, last, count };
  }
}
"@
}
[void][NvlNative]::SetThreadDpiAwarenessContext([IntPtr](-4))

function Get-Win([int]$pidWant) {
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $pidWant)
  return $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
}

function Focus-Wt([int]$pidWant) {
  $win = Get-Win $pidWant
  if (-not $win) { return $false }
  $h = [IntPtr]$win.Current.NativeWindowHandle
  [void][NvlNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
  [void][NvlNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
  [void][NvlNative]::SetForegroundWindow($h)
  Start-Sleep -Milliseconds 350
  return ([NvlNative]::GetForegroundWindow() -eq $h)
}

# Robust capture while the machine is in use: raise our window to TOPMOST
# (no focus needed) so nothing overlaps it, then CopyFromScreen. PrintWindow
# is not an option: WT renders via DirectX and returns stale frames.
function Raise-Top([IntPtr]$hwnd) {
  [void][NvlNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x43)
}
function Capture-Window([IntPtr]$hwnd, [int]$l, [int]$t, [int]$w, [int]$hh) {
  Raise-Top $hwnd
  Start-Sleep -Milliseconds 200
  $bmp = New-Object System.Drawing.Bitmap($w, $hh)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($l, $t, 0, 0, (New-Object System.Drawing.Size($w, $hh)))
  $g.Dispose()
  return $bmp
}

switch ($Cmd) {
  'type' {
    if (-not (Focus-Wt $ProcId)) {
      [Console]::Error.WriteLine('focus-failed')
      exit 6
    }
    [System.Windows.Forms.SendKeys]::SendWait($Needle + '{ENTER}')
    Start-Sleep -Milliseconds 300
    'TYPED'
  }
  'drag' {
    if (-not (Focus-Wt $ProcId)) { [Console]::Error.WriteLine('warn: focus mismatch, dragging anyway') }
    $win0 = Get-Win $ProcId
    if (-not $win0) { [Console]::Error.WriteLine('no-window'); exit 2 }
    Raise-Top ([IntPtr]$win0.Current.NativeWindowHandle)
    Start-Sleep -Milliseconds 200
    $saved = New-Object NvlNative+POINT
    [void][NvlNative]::GetCursorPos([ref]$saved)
    $ins = New-Object 'NvlNative+INPUT[]' 1
    $sx = 0; $sy = 0
    [NvlNative]::ToAbs($X1, $Y1, [ref]$sx, [ref]$sy)
    $ins[0] = [NvlNative]::Mouse($sx, $sy, ([NvlNative]::F_MOVE -bor [NvlNative]::F_ABS -bor [NvlNative]::F_VIRT))
    [NvlNative]::SendMouse($ins)
    Start-Sleep -Milliseconds 150
    $ins[0] = [NvlNative]::Mouse(0, 0, [NvlNative]::F_LEFTDOWN)
    [NvlNative]::SendMouse($ins)
    Start-Sleep -Milliseconds 150
    $steps = 10
    for ($i = 1; $i -le $steps; $i++) {
      $xx = [int]($X1 + ($X2 - $X1) * $i / $steps)
      $yy = [int]($Y1 + ($Y2 - $Y1) * $i / $steps)
      [NvlNative]::ToAbs($xx, $yy, [ref]$sx, [ref]$sy)
      $ins[0] = [NvlNative]::Mouse($sx, $sy, ([NvlNative]::F_MOVE -bor [NvlNative]::F_ABS -bor [NvlNative]::F_VIRT))
      [NvlNative]::SendMouse($ins)
      Start-Sleep -Milliseconds 40
    }
    Start-Sleep -Milliseconds 150
    $ins[0] = [NvlNative]::Mouse(0, 0, [NvlNative]::F_LEFTUP)
    [NvlNative]::SendMouse($ins)
    Start-Sleep -Milliseconds 150
    [void][NvlNative]::SetCursorPos($saved.X, $saved.Y)
    'DRAGGED'
  }
  'text' {
    $win = Get-Win $ProcId
    if (-not $win) { [Console]::Error.WriteLine('no-window'); exit 2 }
    $tcond = New-Object System.Windows.Automation.PropertyCondition(
      [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true)
    $els = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tcond)
    foreach ($el in $els) {
      try { $tp = [System.Windows.Automation.TextPattern]$el.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern) } catch { continue }
      try { $txt = $tp.DocumentRange.GetText(60000) } catch { continue }
      if ($txt -and $txt.Length -gt 200) {
        $txt -replace "`r`n", "|" | ForEach-Object { $_.Substring(0, [Math]::Min(1500, $_.Length)) }
        break
      }
    }
    'TEXT-DONE'
  }
  'findbar' {
    $win = Get-Win $ProcId
    if (-not $win) { [Console]::Error.WriteLine('no-window'); exit 2 }
    $h = [IntPtr]$win.Current.NativeWindowHandle
    $rc = New-Object NvlNative+RECT
    [void][NvlNative]::GetWindowRect($h, [ref]$rc)
    $w = $rc.Right - $rc.Left
    $hh = $rc.Bottom - $rc.Top
    if ($w -le 0 -or $hh -le 0) { [Console]::Error.WriteLine('bad-rect'); exit 5 }
    $bmp = Capture-Window $h $rc.Left $rc.Top $w $hh
    $bd = $bmp.LockBits(
      (New-Object System.Drawing.Rectangle(0, 0, $w, $hh)),
      [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
      [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $len = [int]($bd.Stride * $hh)
    $buf = New-Object byte[] $len
    [System.Runtime.InteropServices.Marshal]::Copy($bd.Scan0, $buf, 0, $len)
    $stride = $bd.Stride
    $bmp.UnlockBits($bd)
    $bmp.Dispose()

    # Compiled scan: solid Write-Host bars, bg == fg (invisible glyphs).
    # Runs of >= 40 px distinguish them from any text glyphs.
    $dy = [NvlNative]::ScanColor($buf, $stride, $w, $hh, 0)
    $cy = [NvlNative]::ScanColor($buf, $stride, $w, $hh, 1)
    if ((-not $dy) -and (-not $cy)) {
      $hist = @{}
      for ($y = 0; $y -lt $hh; $y += 3) {
        $rowBase = $y * $stride
        for ($x = 0; $x -lt $w; $x += 3) {
          $o = $rowBase + $x * 4
          $key = "{0:x2}{1:x2}{2:x2}" -f $buf[$o + 2], $buf[$o + 1], $buf[$o]
          $hist[$key] = 1 + $hist[$key]
        }
      }
      $top = ($hist.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 10 |
        ForEach-Object { "{0}:{1}" -f $_.Key, $_.Value }) -join ","
      [Console]::Error.WriteLine("no-bars colors=$top")
      exit 4
    }
    function Fmt($a) { if ($a) { "{0},{1},{2},{3}" -f $a[2], $a[0], ($a[3] - $a[2] + 1), ($a[1] - $a[0] + 1) } else { "none" } }
    "BARS dy={0};cy={1}" -f (Fmt $dy), (Fmt $cy)
  }
  'rect' {
    $win = Get-Win $ProcId
    if (-not $win) { [Console]::Error.WriteLine('no-window'); exit 2 }
    $h = [IntPtr]$win.Current.NativeWindowHandle
    $rc = New-Object NvlNative+RECT
    [void][NvlNative]::GetWindowRect($h, [ref]$rc)
    "RECT {0} {1} {2} {3}" -f $rc.Left, $rc.Top, $rc.Right, $rc.Bottom
  }
  'sample' {
    $win = Get-Win $ProcId
    if (-not $win) { [Console]::Error.WriteLine('no-window'); exit 2 }
    $h = [IntPtr]$win.Current.NativeWindowHandle
    $rc = New-Object NvlNative+RECT
    [void][NvlNative]::GetWindowRect($h, [ref]$rc)
    $w = $rc.Right - $rc.Left
    $hh = $rc.Bottom - $rc.Top
    if ($w -le 0 -or $hh -le 0) { [Console]::Error.WriteLine('bad-rect'); exit 5 }
    $bmp = Capture-Window $h $rc.Left $rc.Top $w $hh
    $bd = $bmp.LockBits(
      (New-Object System.Drawing.Rectangle(0, 0, $w, $hh)),
      [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
      [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $len = [int]($bd.Stride * $hh)
    $buf = New-Object byte[] $len
    [System.Runtime.InteropServices.Marshal]::Copy($bd.Scan0, $buf, 0, $len)
    $stride = $bd.Stride
    $bmp.UnlockBits($bd)
    if ($Out) { $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
    $r = [NvlNative]::ScanBlue($buf, $stride, $w, $hh, $L, $T, $W, $H)
    "BLUE {0} {1} {2}" -f $r[0], $r[1], $r[2]
  }
  default { [Console]::Error.WriteLine("unknown cmd: $Cmd"); exit 1 }
}
"##;

#[derive(Debug)]
struct Bar {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Runs one helper subcommand; kills PowerShell on a 40 s hang. Returns
/// (stdout, stderr).
fn run_helper(ps_path: &str, args: &[String]) -> Result<(String, String), String> {
    let mut child = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            ps_path,
        ])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn powershell: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child.wait_with_output().expect("wait_with_output");
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !out.status.success() {
                    return Err(format!("helper {args:?} failed: stderr={stderr:?}"));
                }
                return Ok((stdout, stderr));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(format!("helper {args:?} timed out after 40 s"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("helper wait: {e}")),
        }
    }
}

fn find_wt_exe() -> Option<String> {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let known = std::path::Path::new(&local).join("Microsoft\\WindowsApps\\wt.exe");
        if known.exists() {
            return Some(known.display().to_string());
        }
    }
    let out = Command::new("where.exe").arg("wt.exe").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// PIDs of every running WindowsTerminal.exe (best effort; empty on error).
fn wt_pids() -> Vec<u32> {
    let out = match Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq WindowsTerminal.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split('"').nth(3).and_then(|p| p.parse().ok()))
        .collect()
}

fn kill_trees(pids: &[u32]) {
    for pid in pids {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
}

/// Kills the WT process tree spawned for this test (dropped on panics too).
struct WtCleanup {
    kill: Vec<u32>,
}

impl Drop for WtCleanup {
    fn drop(&mut self) {
        kill_trees(&self.kill);
    }
}

fn read_clipboard_now() -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.get_text().ok()
}

/// Type `text` into the WT window of `pid`, retrying while the window is
/// not realized or not focusable yet.
fn type_into(ps_path: &str, pid: u32, text: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match run_helper(
            ps_path,
            &[
                "-Cmd".to_string(),
                "type".to_string(),
                "-ProcId".to_string(),
                pid.to_string(),
                "-Needle".to_string(),
                text.to_string(),
            ],
        ) {
            Ok((line, _)) if line == "TYPED" => return,
            Err(err) if err.contains("no-window") || err.contains("focus-failed") => {}
            Err(err) => panic!("type failed: {err}"),
            Ok((_, _)) => {}
        }
        assert!(
            Instant::now() < deadline,
            "WT window for pid {pid} never accepted typing"
        );
        std::thread::sleep(Duration::from_millis(600));
    }
}

/// Poll the helper until both anchor bars are visible in the WT window of
/// `pid`; returns (dark_yellow, cyan).
fn wait_anchor(ps_path: &str, pid: u32, secs: u64) -> (Bar, Bar) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match run_helper(
            ps_path,
            &[
                "-Cmd".to_string(),
                "findbar".to_string(),
                "-ProcId".to_string(),
                pid.to_string(),
            ],
        ) {
            Ok((line, _)) => {
                if let Some(rest) = line.strip_prefix("BARS ") {
                    let parse = |spec: &str| -> Option<Bar> {
                        let nums: Vec<i32> =
                            spec.split(',').filter_map(|t| t.parse().ok()).collect();
                        (nums.len() == 4).then(|| Bar {
                            x: nums[0],
                            y: nums[1],
                            w: nums[2],
                            h: nums[3],
                        })
                    };
                    let dy = rest
                        .split(";cy=")
                        .next()
                        .and_then(|s| s.strip_prefix("dy="))
                        .and_then(parse);
                    let cy = rest.split(";cy=").nth(1).and_then(parse);
                    if let (Some(dy), Some(cy)) = (dy, cy) {
                        return (dy, cy);
                    }
                }
            }
            Err(err) if err.contains("no-window") || err.contains("no-bars") => {}
            Err(err) => panic!("findbar failed: {err}"),
        }
        if Instant::now() >= deadline {
            let tab = run_helper(
                ps_path,
                &[
                    "-Cmd".to_string(),
                    "text".to_string(),
                    "-ProcId".to_string(),
                    pid.to_string(),
                ],
            )
            .map(|(o, _)| o)
            .unwrap_or_else(|e| format!("<text dump failed: {e}>"));
            panic!("anchor bars never appeared in the WT window of pid {pid}; tab text: {tab}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "slow tier: real Windows Terminal + SendInput; run with -- --ignored --test-threads=1"]
fn wt_drag_selects_span_copies_and_paints() {
    if !cfg!(windows) {
        eprintln!("Windows only — skipping");
        return;
    }
    let Some(wt) = find_wt_exe() else {
        eprintln!("wt.exe not found on this host — skipping (needs Windows Terminal)");
        return;
    };
    let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..\\..\\target\\release-dev\\noviewlog-tui.exe");
    assert!(
        exe.exists(),
        "release-dev binary missing at {}: run `cargo build --profile release-dev -p noviewlog-tui` first (product path only, never debug)",
        exe.display()
    );

    let ps_path = std::env::temp_dir().join("nvl-wt-e2e-helper.ps1");
    std::fs::write(&ps_path, HELPER_PS).expect("write helper ps1");
    let ps_path = ps_path.display().to_string();

    let sentinel = format!(
        "NVL{}{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis()
    );
    let len = sentinel.chars().count();
    let k = len / 2;

    // One typed line paints both anchor bars and puts the sentinel text on
    // the row right below the cyan bar (echo follows the last write-host).
    let anchor_cmd = concat!(
        "$b='#'*80;",
        "write-host $b -b darkyellow -f darkyellow;",
        "write-host $b -b darkcyan -f darkcyan;",
    );

    let baseline = wt_pids();
    // wt.exe is a launcher: it exits as soon as the terminal window process
    // takes over, and the window is torn down by WtCleanup via taskkill.
    let mut wt_launcher = Command::new(&wt)
        .args(["-w", "new", "nt", "--title", "nvl-e2e"])
        .arg(exe.display().to_string())
        .spawn()
        .expect("spawn wt.exe");
    let _ = wt_launcher.wait();

    // Wait for a new WindowsTerminal process (our window). If `-w new` ever
    // merges into an existing window instead, no new pid shows up: fail
    // with a clear message rather than touch the user's window.
    let deadline = Instant::now() + Duration::from_secs(25);
    let spawned = loop {
        let new: Vec<u32> = wt_pids()
            .into_iter()
            .filter(|p| !baseline.contains(p))
            .collect();
        if !new.is_empty() {
            break new;
        }
        assert!(
            Instant::now() < deadline,
            "no new WindowsTerminal process appeared in 25 s (is wt.exe forwarding tabs into an existing window?)"
        );
        std::thread::sleep(Duration::from_millis(400));
    };
    let cleanup = WtCleanup {
        kill: spawned.clone(),
    };
    let win_pid = spawned[spawned.len() - 1];

    // The anchors double as the ready gate: they only render once the TUI
    // is up AND the local shell executed the command.
    let mut full_cmd = anchor_cmd.to_string();
    full_cmd.push_str(&format!("echo {sentinel}"));
    type_into(&ps_path, win_pid, &full_cmd, 20);
    let (dy, cy) = wait_anchor(&ps_path, win_pid, 20);
    assert!(
        cy.w > 400 && cy.h > 5,
        "cyan anchor bar degenerate: {cy:?} (80 chars expected)"
    );
    assert!(
        dy.y < cy.y,
        "dark-yellow anchor below the cyan bar: dy={dy:?} cy={cy:?}"
    );

    // The sentinel sits exactly on the row after the cyan bar (echo output
    // follows the last write-host row), starting at content column 0. The
    // bar clusters carry ink extents, not cell boxes, so the row pitch comes
    // from the bar spacing (the two bars occupy consecutive rows) and the
    // sentinel row origin is one pitch below the cyan bar ink.
    let cw = cy.w as f64 / 80.0;
    let pitch = cy.y - dy.y;
    let sentinel_y = cy.y + pitch;
    let mid_y = sentinel_y + pitch / 2;

    // Window rect in screen coords. Anchor coords are capture-relative and
    // the capture starts at the window rect origin, so screen = rect + local.
    let (rect_line, _) = run_helper(
        &ps_path,
        &[
            "-Cmd".to_string(),
            "rect".to_string(),
            "-ProcId".to_string(),
            win_pid.to_string(),
        ],
    )
    .expect("window rect via GetWindowRect");
    let win_nums: Vec<i32> = rect_line
        .strip_prefix("RECT ")
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    let [wl, wt, wr, wb] = win_nums.as_slice() else {
        panic!("unexpected rect output: {rect_line:?}");
    };
    assert!(
        wr > wl && wb > wt && *wl > -30000 && *wt > -30000,
        "window rect looks off-screen: {rect_line}"
    );
    let (wl, wt) = (*wl as f64, *wt as f64);

    // Drag from the center of sentinel cell 0 to the center of cell k — the
    // same physical gesture the ConPTY suite models: paint covers cells
    // [0, k), the copied text is end-inclusive ([0, k]). SendInput needs
    // screen coordinates.
    let x1 = wl + cy.x as f64 + 0.5 * cw;
    let x2 = wl + cy.x as f64 + (k as f64 + 0.5) * cw;
    let drag_y = wt + mid_y as f64;
    let dragged = run_helper(
        &ps_path,
        &[
            "-Cmd".to_string(),
            "drag".to_string(),
            "-ProcId".to_string(),
            win_pid.to_string(),
            "-X1".to_string(),
            format!("{x1:.0}"),
            "-Y1".to_string(),
            format!("{drag_y:.0}"),
            "-X2".to_string(),
            format!("{x2:.0}"),
            "-Y2".to_string(),
            format!("{drag_y:.0}"),
        ],
    )
    .expect("drag via SendInput")
    .0;
    assert_eq!(dragged, "DRAGGED");

    // (a) Clipboard: end-inclusive quirk — k+1 chars.
    let expected: String = sentinel.chars().take(k + 1).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = read_clipboard_now();
    while got.as_deref() != Some(expected.as_str()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        got = read_clipboard_now();
    }
    assert_eq!(
        got.as_deref(),
        Some(expected.as_str()),
        "clipboard must hold {expected:?} after the WT mouse-up"
    );

    // (b) Painted highlight extent: sample the sentinel row band, extended
    // ~12 cells past the selection end so a stretch-to-EOL regression is
    // caught. first/last are x offsets inside the band.
    let band_w = ((k as f64 + 12.0) * cw).ceil() as i32;
    let band_h = pitch;
    let png = std::env::temp_dir().join("nvl-wt-e2e-band.png");
    let sampled = run_helper(
        &ps_path,
        &[
            "-Cmd".to_string(),
            "sample".to_string(),
            "-ProcId".to_string(),
            win_pid.to_string(),
            "-L".to_string(),
            cy.x.to_string(),
            "-T".to_string(),
            sentinel_y.to_string(),
            "-W".to_string(),
            band_w.to_string(),
            "-H".to_string(),
            band_h.to_string(),
            "-Out".to_string(),
            png.display().to_string(),
        ],
    )
    .expect("pixel sampling via CopyFromScreen")
    .0;
    let nums: Vec<i64> = sampled
        .strip_prefix("BLUE ")
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    let [first, last, count] = nums.as_slice() else {
        panic!("unexpected sample output: {sampled:?}");
    };
    assert!(
        *count > 0,
        "no selection-blue pixels in the band (sampled: {sampled}, png: {})",
        png.display()
    );
    assert!(
        *first >= -2 && *first <= (cw + 2.0) as i64,
        "highlight must start at the sentinel start (first blue at band x={first}, cw={cw})"
    );
    let lo = ((k - 1) as f64 * cw - 3.0) as i64;
    let hi = (k as f64 * cw + 1.0) as i64;
    assert!(
        *last >= lo && *last <= hi,
        "highlight must cover exactly cells [0, {k}): last blue at band x={last}, expected {lo}..{hi} (cw={cw}, png: {})",
        png.display()
    );

    drop(cleanup);
}

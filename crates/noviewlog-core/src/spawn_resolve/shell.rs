#[cfg(windows)]
use super::windows::find_windows_executable;
use crate::core::types::ShellPreference;

/// Executable name for an interactive shell (issue #68). Pure on explicit
/// preferences; `auto`/`pwsh` probe PATH for PowerShell 7 on Windows.
pub(super) fn default_interactive_shell(_shell: ShellPreference) -> String {
    #[cfg(windows)]
    {
        match _shell {
            ShellPreference::Cmd => "cmd.exe".to_string(),
            ShellPreference::Powershell => "powershell.exe".to_string(),
            // Explicit pwsh keeps the name even when missing, so the spawn
            // error tells the user PowerShell 7 is not installed.
            ShellPreference::Pwsh => "pwsh.exe".to_string(),
            ShellPreference::Auto => {
                if pwsh_available() {
                    "pwsh.exe".to_string()
                } else {
                    "powershell.exe".to_string()
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        // Unix shells are unaffected by the Windows shell preference.
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

#[cfg(windows)]
fn pwsh_available() -> bool {
    // PATH probing scans the registry and every PATH dir — cache after the
    // first check (issue #113): result effectively never changes mid-session.
    //
    // The caller may be the UI tick, so it must never block on the scan
    // (issue #59 review): while the probe is in flight every caller gets the
    // safe `false` (→ `powershell.exe`) and later starts pick up the real
    // result. Tests keep the synchronous probe — deterministic resolution
    // beats never-blocking when nothing is on a UI deadline.
    use std::sync::OnceLock;
    static CACHE: OnceLock<bool> = OnceLock::new();
    #[cfg(test)]
    {
        // Tests keep the synchronous probe — deterministic resolution beats
        // never-blocking when nothing is on a UI deadline.
        *CACHE.get_or_init(|| find_windows_executable("pwsh", None).is_some())
    }
    #[cfg(not(test))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static PROBE_STARTED: AtomicBool = AtomicBool::new(false);
        if let Some(available) = CACHE.get() {
            return *available;
        }
        if !PROBE_STARTED.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let available = find_windows_executable("pwsh", None).is_some();
                let _ = CACHE.set(available);
            });
        }
        false
    }
}

/// Kick the `pwsh` probe (and the registry-PATH cache) on a worker thread
/// (issue #59): `auto`-shell resolution never scans PATH on the UI thread —
/// while the probe is in flight it falls back to `powershell.exe`, and
/// later starts pick up `pwsh.exe` once the probe has landed.
#[cfg(windows)]
pub fn prewarm_shell_probe() {
    std::thread::spawn(pwsh_available);
}

/// No pwsh probe off Windows: `auto` shells resolve directly on POSIX.
#[cfg(not(windows))]
pub fn prewarm_shell_probe() {}

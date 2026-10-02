/**
 * PTY wrapper: resolves the command through the platform shell and owns the
 * node-pty process handle. The prebuilt NAPI distribution keeps packaging
 * free of node-gyp toolchains.
 */

import type { IPty } from "@lydell/node-pty";

export interface PtyEvents {
  onData: (data: string) => void;
  onExit: (status: string) => void;
}

/** Launch `command` in a PTY rooted at `cwd` with the given grid size. */
export function spawnPty(
  command: string,
  cwd: string,
  cols: number,
  rows: number,
  events: PtyEvents,
): { resize: (cols: number, rows: number) => void; write: (data: string) => void; kill: () => void } {
  // Lazy require: the native module must stay external to the esbuild bundle.
  const pty: typeof import("@lydell/node-pty") = require("@lydell/node-pty");

  const isWindows = process.platform === "win32";
  const file = isWindows ? process.env.COMSPEC ?? "cmd.exe" : process.env.SHELL ?? "/bin/sh";
  // An empty command means an interactive login shell (auto-start), not
  // `$SHELL -lc ""` which would exit immediately.
  const interactive = command.trim() === "";
  const args = isWindows
    ? interactive
      ? []
      : ["/c", command]
    : interactive
      ? ["-l"]
      : ["-lc", command];

  const proc: IPty = pty.spawn(file, args, {
    name: "xterm-256color",
    cols,
    rows,
    cwd,
    env: process.env as { [key: string]: string },
  });

  proc.onData(events.onData);
  let killed = false;
  proc.onExit(({ exitCode, signal }) => {
    killed = true;
    const status = exitCode !== 0 ? `exit code ${exitCode}` : signal ? `signal ${signal}` : "exit code 0";
    events.onExit(status);
  });

  return {
    resize: (c, r) => {
      // A resize racing the process exit throws inside node-pty; it is
      // meaningless for a dead process, so swallow it.
      if (!killed) {
        try {
          proc.resize(c, r);
        } catch {
          // already exited
        }
      }
    },
    write: (data) => {
      if (killed) {
        return;
      }
      try {
        proc.write(data);
      } catch {
        // process exited mid-write; the exit event reports it
      }
    },
    kill: () => {
      if (!killed) {
        killed = true;
        try {
          proc.kill();
        } catch {
          // already gone
        }
      }
    },
  };
}

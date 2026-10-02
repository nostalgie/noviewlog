/**
 * Smoke tests inside a real (headless-capable) VS Code instance, run by
 * `npm run test:vscode` (@vscode/test-cli). Catches the classes of breakage
 * that plain Node tests cannot see: native module ABI mismatches against
 * the shipped Electron, activation errors, and command registration.
 * Plain mocha — the extension host provides `describe`/`it`.
 */

const assert = require("node:assert");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");

const EXT_ID = "noviewlog.noviewlog-vscode";
const COMMANDS = [
  "noviewlog.runCommand",
  "noviewlog.openLogFile",
  "noviewlog.openFileHere",
  "noviewlog.stop",
  "noviewlog.restart",
];

suite("NoViewLog extension (inside VS Code)", () => {
  /** @type {import('vscode')} */
  let vscode;
  suiteSetup(async () => {
    vscode = require("vscode");
    const ext = vscode.extensions.getExtension(EXT_ID);
    assert.ok(ext, `extension ${EXT_ID} is not present`);
    await ext.activate();
  });

  test("activates without errors", () => {
    assert.ok(vscode.extensions.getExtension(EXT_ID).isActive);
  });

  test("registers all user-facing commands", async () => {
    const registered = new Set(await vscode.commands.getCommands(true));
    for (const cmd of COMMANDS) {
      assert.ok(registered.has(cmd), `command missing: ${cmd}`);
    }
  });

  test("loads and drives node-pty against the host Electron ABI", function () {
    this.timeout(15_000);
    const pty = require("@lydell/node-pty");
    return new Promise((resolve, reject) => {
      const shell = process.platform === "win32" ? "cmd.exe" : "/bin/sh";
      const args = process.platform === "win32" ? ["/c", "echo abi-ok"] : ["-c", "echo abi-ok"];
      const proc = pty.spawn(shell, args, { cols: 80, rows: 24 });
      let out = "";
      proc.onData((d) => {
        out += d;
      });
      proc.onExit(({ exitCode }) => {
        if (exitCode === 0 && out.includes("abi-ok")) {
          resolve();
        } else {
          reject(new Error(`pty smoke failed (exit ${exitCode}, out ${JSON.stringify(out.slice(0, 80))})`));
        }
      });
    });
  });

  test("opens a log-file session without errors", async function () {
    this.timeout(15_000);
    const file = path.join(os.tmpdir(), `noviewlog-smoke-${process.pid}.log`);
    fs.writeFileSync(file, "line 0 INFO\nline 1 ERROR\n");
    try {
      await vscode.commands.executeCommand("noviewlog.openFileHere", vscode.Uri.file(file));
    } finally {
      await vscode.commands.executeCommand("noviewlog.stop");
      fs.rmSync(file, { force: true });
    }
  });
});

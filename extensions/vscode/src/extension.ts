/**
 * Extension entry: commands, session UX (Run Command QuickPick with
 * history, Open Log File), and wiring between the engine host and the two
 * webview surfaces.
 */

import * as vscode from "vscode";
import { EngineHost, type SavedTabs } from "./engineHost";
import { Surfaces } from "./surfaces";

const HISTORY_KEY = "noviewlog.commandHistory";
const TABS_KEY = "noviewlog.workspaceTabs";
const PANEL_WIDTH_KEY = "noviewlog.panelWidth";
const MAX_HISTORY = 10;
const DEFAULT_PANEL_WIDTH = 300;

export function activate(context: vscode.ExtensionContext): void {
  const engineHost = new EngineHost();
  const surfaces = new Surfaces(context.extensionUri);

  surfaces.onCommand = (cmd) => {
    if (cmd.type === "ready") {
      // One-shot per webview attach: the preset list never changes while
      // the webview DOM lives, so it rides along with the first snapshot.
      const presets = engineHost.presetsMessage();
      if (presets) {
        surfaces.post(presets);
      }
    }
    engineHost.handleCommand(cmd);
  };
  // First reveal of any surface: desktop parity (project-open-autostart) —
  // an interactive shell starts unless one already exists or the user
  // turned noviewlog.autoStart off.
  let autoStarted = false;
  surfaces.onReveal = () => {
    engineHost.sendSnapshot();
    if (autoStarted || !autoStartConfig()) {
      return;
    }
    autoStarted = true;
    if (engineHost.active === null) {
      engineHost.startPty("", workspaceRoot());
    }
  };
  surfaces.onResize = (cols, rows) => engineHost.resizeSession(cols, rows);
  surfaces.setPanelWidthStorage({
    load: () => context.workspaceState.get<number>(PANEL_WIDTH_KEY) ?? DEFAULT_PANEL_WIDTH,
    save: (width) => void context.workspaceState.update(PANEL_WIDTH_KEY, width),
  });
  engineHost.setPublisher((msg) => {
    // No visibility gate here: Surfaces.post already checks each surface,
    // and dropping appends while both surfaces are momentarily hidden would
    // desync any mid-render webview.
    surfaces.post(msg);
  });
  engineHost.setPersistence({
    load: () => context.workspaceState.get<SavedTabs>(TABS_KEY),
    save: (tabs) => void context.workspaceState.update(TABS_KEY, tabs),
  });
  engineHost.onSessionChanged = () => {
    void vscode.commands.executeCommand(
      "setContext",
      "noviewlog.sessionActive",
      engineHost.active !== null,
    );
  };
  engineHost.setMaxRecords(maxRecordsConfig());

  context.subscriptions.push(engineHost, surfaces);

  const provider = surfaces.registerViewProvider();
  context.subscriptions.push(provider);
  surfaces.registerPanel(context);

  // Context-menu gate: resourceExtname in noviewlog.logExtensions.
  const setContextKeys = () => {
    void vscode.commands.executeCommand(
      "setContext",
      "noviewlog.logExtensions",
      logExtensionsConfig(),
    );
  };
  setContextKeys();
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("noviewlog")) {
        setContextKeys();
        engineHost.setMaxRecords(maxRecordsConfig());
      }
    }),
  );

  const startPty = async () => {
    const command = await promptCommand(context);
    if (!command) {
      return;
    }
    const cwd = workspaceRoot();
    surfaces.showPanel();
    engineHost.startPty(command, cwd);
  };

  const startFile = async (uri?: vscode.Uri) => {
    const target = uri ?? (await promptLogFile());
    if (!target) {
      return;
    }
    surfaces.showPanel();
    engineHost.startFile(target.fsPath);
  };

  context.subscriptions.push(
    vscode.commands.registerCommand("noviewlog.runCommand", () => void startPty()),
    vscode.commands.registerCommand("noviewlog.openLogFile", () => void startFile()),
    vscode.commands.registerCommand("noviewlog.openFileHere", (uri: vscode.Uri | undefined) =>
      void startFile(uri),
    ),
    vscode.commands.registerCommand("noviewlog.stop", () => engineHost.stopSession()),
    vscode.commands.registerCommand("noviewlog.restart", () => engineHost.restart()),
  );
}

export function deactivate(): void {
  // Disposables (engine host incl. PTY, surfaces) clean up via subscriptions.
}

function logExtensionsConfig(): string[] {
  const config = vscode.workspace.getConfiguration("noviewlog");
  const exts = config.get<string[]>("logExtensions", [".log", ".txt"]);
  return exts.map((e) => (e.startsWith(".") ? e : `.${e}`));
}

function maxRecordsConfig(): number {
  const config = vscode.workspace.getConfiguration("noviewlog");
  return config.get<number>("maxScrollbackLines", 10_000);
}

function autoStartConfig(): boolean {
  return vscode.workspace.getConfiguration("noviewlog").get<boolean>("autoStart", true);
}

function workspaceRoot(): string {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? process.cwd();
}

async function promptLogFile(): Promise<vscode.Uri | undefined> {
  const picks = await vscode.window.showOpenDialog({
    canSelectMany: false,
    openLabel: "Open in NoViewLog",
    filters: { "Log files": ["log", "txt", "out"], "All files": ["*"] },
  });
  return picks?.[0];
}

/** QuickPick with persistent history; free-text entry lands in the history. */
async function promptCommand(context: vscode.ExtensionContext): Promise<string | undefined> {
  const history = context.workspaceState.get<string[]>(HISTORY_KEY, []);
  const quickPick = vscode.window.createQuickPick();
  quickPick.placeholder = "Command to run (e.g. npm run dev)";
  quickPick.matchOnDescription = true;
  quickPick.items = history.map((cmd) => ({ label: cmd, description: "recent" }));
  const picked = await new Promise<string | undefined>((resolve) => {
    quickPick.onDidAccept(() => {
      resolve(quickPick.selectedItems[0]?.label ?? (quickPick.value || undefined));
    });
    quickPick.onDidHide(() => resolve(undefined));
    quickPick.show();
  });
  quickPick.dispose();
  if (picked && !history.includes(picked)) {
    const next = [picked, ...history].slice(0, MAX_HISTORY);
    void context.workspaceState.update(HISTORY_KEY, next);
  }
  return picked;
}

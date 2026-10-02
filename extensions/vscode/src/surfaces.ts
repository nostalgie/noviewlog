/**
 * The two webview surfaces (editor WebviewPanel + bottom-panel WebviewView)
 * share one session. Hidden webviews lose their DOM, so on every reveal the
 * host re-sends a full snapshot; commands from the visible surface are
 * routed to the engine host.
 */

import * as vscode from "vscode";
import { type HostToWebview, isWebviewToHost, type WebviewToHost } from "./protocol";

export class Surfaces implements vscode.Disposable {
  private panel: vscode.WebviewPanel | null = null;
  private view: vscode.WebviewView | null = null;

  private readonly disposables: vscode.Disposable[] = [];

  onCommand: ((cmd: WebviewToHost) => void) | null = null;
  onReveal: (() => void) | null = null;
  onResize: ((cols: number, rows: number) => void) | null = null;

  /** Filter panel width persistence (workspaceState-backed). */
  private panelWidth = 300;
  private savePanelWidth: ((width: number) => void) | null = null;

  constructor(private readonly extensionUri: vscode.Uri) {}

  /** Wire workspace storage for the filter panel width. */
  setPanelWidthStorage(storage: {
    load: () => number;
    save: (width: number) => void;
  }): void {
    this.panelWidth = storage.load();
    this.savePanelWidth = storage.save;
  }

  private buildHtml(webview: vscode.Webview): string {
    const script = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, "dist", "webview.js"),
    );
    const nonce = getNonce();
    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy"
      content="default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>NoViewLog</title>
<style nonce="${nonce}">
  :root {
    color-scheme: var(--vscode-coder-color-scheme, light dark);
    --panel-w: ${this.panelWidth}px;
  }
  html, body { height: 100%; margin: 0; overflow: hidden; position: relative; }
  body {
    display: grid;
    grid-template-columns: 1fr auto var(--panel-w, 300px);
    background: var(--vscode-editor-background); color: var(--vscode-editor-foreground);
    font-family: var(--vscode-font-family); font-size: var(--vscode-font-size, 13px);
  }
  #main { display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow: hidden; }
  #chrome { flex: none; position: relative; z-index: 2; }
  #scroller { flex: 1 1 auto; overflow: auto; position: relative; outline: none; }
  #spacer { position: absolute; top: 0; left: 0; width: 1px; pointer-events: none; }
  #canvas { position: sticky; top: 0; display: block; }

  #splitter {
    width: 4px; cursor: col-resize; touch-action: none;
    background: var(--vscode-panel-border, transparent);
  }
  #splitter:hover, #splitter.dragging {
    background: var(--vscode-focusBorder);
  }
  #side {
    min-width: 0; overflow-y: auto; overflow-x: hidden;
    border-left: 1px solid var(--vscode-panel-border, transparent);
    background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
    box-sizing: border-box;
  }
  body.panel-closed { grid-template-columns: 1fr 0 0; }
  body.panel-closed #splitter, body.panel-closed #side { display: none; }

  /* Narrow webview: the open panel overlays the viewport so the log keeps
   * its grid (canvas size stays stable). */
  @media (max-width: 479px) {
    body { grid-template-columns: 1fr; }
    #splitter { display: none; }
    #side {
      position: absolute; top: 0; right: 0; bottom: 0; z-index: 4;
      width: min(var(--panel-w, 300px), 85vw);
      box-shadow: -2px 0 8px rgba(0, 0, 0, 0.35);
    }
  }

  #chrome {
    display: flex; flex-direction: column; gap: 4px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--vscode-panel-border, transparent);
    background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
  }
  #chrome button, #chrome select, #chrome input, #chrome textarea {
    font-family: inherit; font-size: inherit; color: inherit;
  }
  #chrome button {
    padding: 2px 8px;
    background: transparent; color: var(--vscode-foreground);
    border: 1px solid transparent; border-radius: 2px;
    cursor: pointer;
  }
  #chrome button:hover { background: var(--vscode-toolbar-hoverBackground); }
  #chrome button[aria-pressed="true"] {
    background: var(--vscode-button-background);
    color: var(--vscode-button-foreground);
  }
  #chrome button:focus-visible, #chrome input:focus-visible,
  #chrome select:focus-visible, #chrome textarea:focus-visible,
  #side button:focus-visible, #side input:focus-visible, #side select:focus-visible {
    outline: 1px solid var(--vscode-focusBorder); outline-offset: -1px;
  }
  #chrome .tabs { display: flex; gap: 1px; overflow-x: auto; }
  #chrome .tab {
    padding: 3px 10px;
    background: transparent;
    border: none; border-bottom: 1px solid transparent;
    color: var(--vscode-foreground); opacity: 0.75;
  }
  #chrome .tab:hover { opacity: 1; }
  #chrome .tab.active {
    opacity: 1; font-weight: 600;
    border-bottom-color: var(--vscode-focusBorder);
  }
  #chrome .tab.terminal { font-style: italic; }
  #chrome .tab-close {
    margin-left: 6px; opacity: 0.6;
  }
  #chrome .tab-close:hover { opacity: 1; }
  #chrome .controls { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; }
  #chrome .find { display: flex; align-items: center; gap: 2px; }
  #chrome .find input {
    width: 160px; padding: 2px 6px;
    background: var(--vscode-input-background); color: var(--vscode-input-foreground);
    border: 1px solid var(--vscode-input-border, transparent); border-radius: 2px;
  }
  #chrome .find-label { margin-left: 6px; opacity: 0.7; font-size: 12px; }
  #chrome .exit-label {
    padding: 1px 8px; font-size: 12px;
    background: var(--vscode-inputValidation-errorBackground, rgba(160, 60, 60, 0.35));
    border-radius: 2px;
  }

  /* ----- filter panel (fp- classes are owned by filtersPanel.ts) ----- */
  #side { padding: 8px; }
  #side button, #side select, #side input {
    font-family: inherit; font-size: inherit; color: inherit;
  }
  #side button {
    padding: 2px 8px;
    background: transparent; color: var(--vscode-foreground);
    border: 1px solid transparent; border-radius: 2px;
    cursor: pointer;
  }
  #side button:hover:not(:disabled) { background: var(--vscode-toolbar-hoverBackground); }
  #side button:disabled { opacity: 0.5; cursor: default; }
  #side button[aria-pressed="true"] {
    background: var(--vscode-button-background);
    color: var(--vscode-button-foreground);
  }
  #side select, #side input[type="text"] {
    padding: 2px 4px;
    background: var(--vscode-input-background); color: var(--vscode-input-foreground);
    border: 1px solid var(--vscode-input-border, transparent); border-radius: 2px;
  }
  #side input[type="checkbox"] { accent-color: var(--vscode-button-background); }
  .fp-head {
    display: flex; align-items: center; gap: 6px;
    margin-bottom: 8px;
  }
  .fp-title { font-weight: 600; flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fp-rename {
    flex: 1 1 auto; min-width: 0; width: 100%;
  }
  .fp-readonly-badge {
    flex: none; padding: 1px 8px; font-size: 11px; font-style: italic;
    background: var(--vscode-badge-background); color: var(--vscode-badge-foreground);
    border-radius: 8px;
  }
  .fp-close { flex: none; opacity: 0.7; }
  .fp-close:hover { opacity: 1; }
  .fp-section { margin-bottom: 12px; }
  .fp-section-title {
    font-size: 11px; font-weight: 600; letter-spacing: 0.4px;
    opacity: 0.7; margin: 0 0 4px 0;
    text-transform: uppercase;
  }
  .fp-rule {
    display: flex; align-items: center; gap: 4px;
    margin-bottom: 4px;
  }
  .fp-rule select { flex: none; }
  .fp-rule input[type="text"] {
    flex: 1 1 40px; min-width: 30px;
    font-family: var(--vscode-editor-font-family, monospace);
  }
  .fp-rule input[type="text"].fp-invalid {
    border-color: var(--vscode-inputValidation-errorBorder, var(--vscode-errorForeground, red));
    background: var(--vscode-inputValidation-errorBackground, rgba(160, 60, 60, 0.25));
  }
  .fp-rule .fp-del { flex: none; opacity: 0.6; }
  .fp-rule .fp-del:hover { opacity: 1; }
  .fp-error {
    font-size: 11px; color: var(--vscode-errorForeground, #f66);
    margin: -2px 0 4px 0; overflow-wrap: anywhere;
  }
  .fp-locked-hint { opacity: 0.75; font-size: 12px; }
  .fp-add { margin-top: 2px; }
  .fp-preset {
    display: block; width: 100%; text-align: left;
    margin-bottom: 2px;
  }
  .fp-seg { display: flex; flex-wrap: wrap; gap: 2px; }
  .fp-tabrow {
    display: flex; align-items: center; gap: 4px;
    margin-bottom: 2px;
  }
  .fp-tabrow .fp-tab-name {
    flex: 1 1 auto; min-width: 0; text-align: left;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .fp-tabrow.active .fp-tab-name { font-weight: 600; }
  .fp-tabrow .fp-tab-close { flex: none; opacity: 0.6; }
  .fp-tabrow .fp-tab-close:hover { opacity: 1; }
  .fp-empty { opacity: 0.6; font-size: 12px; }
  #empty-state {
    position: absolute; inset: 0; z-index: 1;
    display: flex; flex-direction: column; justify-content: center; align-items: center;
    gap: 8px; text-align: center;
    background: var(--vscode-editor-background); color: var(--vscode-editor-foreground);
  }
  #empty-state .empty-title { font-size: 16px; font-weight: 600; }
  #empty-state .empty-hint { opacity: 0.75; }
  #empty-state ul { list-style: none; margin: 0; padding: 0; opacity: 0.9; }
</style>
</head>
<body>
  <div id="main">
    <div id="chrome"></div>
    <div id="scroller"><div id="spacer"></div><canvas id="canvas"></canvas></div>
  </div>
  <div id="splitter" role="separator" aria-orientation="vertical" aria-label="Resize filter panel"></div>
  <div id="side"></div>
  <script nonce="${nonce}" src="${script}"></script>
</body>
</html>`;
  }

  private wire(webview: vscode.Webview): void {
    webview.onDidReceiveMessage((data: unknown) => {
      if (isWebviewToHost(data)) {
        if (data.type === "resize") {
          this.onResize?.(data.cols, data.rows);
        } else if (data.type === "savePanelWidth") {
          this.panelWidth = data.width;
          this.savePanelWidth?.(data.width);
        } else {
          this.onCommand?.(data);
        }
      }
    }, undefined, this.disposables);
  }

  registerPanel(context: vscode.ExtensionContext): void {
    context.subscriptions.push(
      vscode.window.registerWebviewPanelSerializer("noviewlog.panel", {
        deserializeWebviewPanel: async (panel) => {
          this.attachPanel(panel);
        },
      }),
    );
  }

  attachPanel(panel: vscode.WebviewPanel): void {
    this.panel = panel;
    // Fresh HTML per attach: the asWebviewUri resource authority is
    // webview-specific, so one cached document breaks the other surface.
    panel.webview.html = this.buildHtml(panel.webview);
    this.wire(panel.webview);
    // Panel-scoped listeners die with the panel; keeping them out of the
    // shared `disposables` avoids accumulation across serializer re-attaches.
    panel.onDidDispose(() => {
      if (this.panel === panel) {
        this.panel = null;
      }
    });
    // Fires for show and hide alike; only a shown panel needs the snapshot.
    panel.onDidChangeViewState(() => {
      if (panel.visible) {
        this.reveal();
      }
    });
    this.reveal();
  }

  showPanel(): void {
    if (this.panel) {
      this.panel.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      "noviewlog.panel",
      "NoViewLog",
      vscode.ViewColumn.One,
      {
        enableScripts: true,
        retainContextWhenHidden: false,
      },
    );
    this.attachPanel(panel);
  }

  registerViewProvider(): vscode.Disposable {
    const provider = new (class implements vscode.WebviewViewProvider {
      constructor(private surfaces: Surfaces) {}
      resolveWebviewView(view: vscode.WebviewView): void {
        this.surfaces.attachView(view);
      }
    })(this);
    return vscode.window.registerWebviewViewProvider("noviewlog.session", provider, {
      webviewOptions: { retainContextWhenHidden: false },
    });
  }

  attachView(view: vscode.WebviewView): void {
    this.view = view;
    // Unlike a WebviewPanel (enableScripts at creation), a WebviewView's
    // scripts stay disabled until options enable them — set before the HTML.
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, "dist")],
    };
    view.webview.html = this.buildHtml(view.webview);
    this.wire(view.webview);
    view.onDidDispose(() => {
      if (this.view === view) {
        this.view = null;
      }
    }, undefined, this.disposables);
    // Both visibility events also fire on hide; only a shown surface needs
    // the (buffer-sized) full snapshot.
    view.onDidChangeVisibility(() => {
      if (view.visible) {
        this.reveal();
      }
    }, undefined, this.disposables);
    this.reveal();
  }

  /** A surface became visible: fresh DOM, so it needs a full snapshot. */
  private reveal(): void {
    this.onReveal?.();
  }

  get visible(): boolean {
    return (
      (this.panel?.visible ?? false) ||
      (this.view?.visible ?? false)
    );
  }

  /** Send to every visible surface — a surface hidden behind another one
   * (e.g. the editor tab while the bottom panel is focused) is still
   * `visible` in the VS Code API, so fan-out keeps both in sync. */
  post(msg: HostToWebview): void {
    if (this.panel?.visible) {
      void this.panel.webview.postMessage(msg);
    }
    if (this.view?.visible) {
      void this.view.webview.postMessage(msg);
    }
  }

  dispose(): void {
    this.disposables.forEach((d) => d.dispose());
    this.panel?.dispose();
  }
}

function getNonce(): string {
  let text = "";
  const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}

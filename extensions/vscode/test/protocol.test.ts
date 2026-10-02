import { describe, expect, it } from "vitest";
import { isHostToWebview, isWebviewToHost } from "../src/protocol";

describe("isWebviewToHost", () => {
  it("accepts known command types", () => {
    expect(isWebviewToHost({ type: "ready" })).toBe(true);
    expect(isWebviewToHost({ type: "setSearch", query: "x", regex: false, caseSensitive: false, wholeWord: false })).toBe(true);
    expect(isWebviewToHost({ type: "toggleCollapse", recordId: 3 })).toBe(true);
    expect(isWebviewToHost({ type: "renameTab", index: 1, name: "Errors" })).toBe(true);
    expect(isWebviewToHost({ type: "savePanelWidth", width: 320 })).toBe(true);
  });

  it("rejects unknown or malformed messages", () => {
    expect(isWebviewToHost(null)).toBe(false);
    expect(isWebviewToHost("ready")).toBe(false);
    expect(isWebviewToHost({})).toBe(false);
    expect(isWebviewToHost({ type: "snapshot" })).toBe(false);
    expect(isWebviewToHost({ type: "presets", presets: [] })).toBe(false);
    expect(isWebviewToHost({ type: "exec" })).toBe(false);
  });
});

describe("isHostToWebview", () => {
  it("accepts the additive presets message", () => {
    expect(isHostToWebview({ type: "presets", presets: [{ id: "go-errors", filters: [] }] })).toBe(true);
  });

  it("rejects malformed presets messages", () => {
    expect(isHostToWebview({ type: "presets" })).toBe(false);
    expect(isHostToWebview({ type: "presets", presets: "all" })).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import { joinSelectionSlices } from "../src/webview/renderer";

describe("joinSelectionSlices", () => {
  it("returns empty for no slices", () => {
    expect(joinSelectionSlices([])).toBe("");
  });

  it("concatenates soft-wrap rows of the same logical line without newlines", () => {
    // One logical line wrapped into three visual rows (issue #329).
    expect(
      joinSelectionSlices([
        { line: 0, text: "abcd" },
        { line: 0, text: "efgh" },
        { line: 0, text: "ij" },
      ]),
    ).toBe("abcdefghij");
  });

  it("emits a newline only when the logical line changes", () => {
    expect(
      joinSelectionSlices([
        { line: 0, text: "hello" },
        { line: 0, text: " world" },
        { line: 1, text: "next" },
        { line: 1, text: " line" },
      ]),
    ).toBe("hello world\nnext line");
  });

  it("keeps a newline between unwrapped single-row lines", () => {
    expect(
      joinSelectionSlices([
        { line: 0, text: "a" },
        { line: 1, text: "b" },
        { line: 2, text: "c" },
      ]),
    ).toBe("a\nb\nc");
  });
});

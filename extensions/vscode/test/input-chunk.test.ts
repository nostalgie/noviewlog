import { describe, expect, it } from "vitest";
import { MAX_INPUT_CHARS, chunkInputData } from "../src/protocol";

describe("chunkInputData", () => {
  it("returns empty for empty input", () => {
    expect(chunkInputData("")).toEqual([]);
  });

  it("keeps a short payload as one chunk", () => {
    expect(chunkInputData("hello")).toEqual(["hello"]);
  });

  it("splits at MAX_INPUT_CHARS so nothing is truncated", () => {
    const text = "a".repeat(MAX_INPUT_CHARS + 10);
    const chunks = chunkInputData(text);
    expect(chunks).toHaveLength(2);
    expect(chunks[0]).toHaveLength(MAX_INPUT_CHARS);
    expect(chunks[1]).toHaveLength(10);
    expect(chunks.join("")).toBe(text);
  });

  it("keeps bracketed-paste markers across chunk boundaries", () => {
    const body = "x".repeat(MAX_INPUT_CHARS);
    const payload = `\x1b[200~${body}\x1b[201~`;
    const chunks = chunkInputData(payload);
    expect(chunks.join("")).toBe(payload);
    expect(chunks.every((c) => c.length <= MAX_INPUT_CHARS)).toBe(true);
    expect(chunks[0].startsWith("\x1b[200~")).toBe(true);
    expect(chunks[chunks.length - 1].endsWith("\x1b[201~")).toBe(true);
  });
});

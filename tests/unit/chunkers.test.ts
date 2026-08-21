import { describe, it, expect } from "vitest";
import {
  chunkMarkdown,
  chunkCode,
  detectLanguage,
  type Chunk,
} from "../../src/ingest/chunkers.js";

function repeatLine(text: string, times: number): string {
  return Array.from({ length: times }, () => text).join("\n");
}

describe("chunkMarkdown", () => {
  it("returns a single null-path chunk for body without headings", () => {
    const chunks = chunkMarkdown("Just some prose.\n\nSecond paragraph.");
    expect(chunks).toHaveLength(1);
    expect(chunks[0]).toMatchObject({ ordinal: 0, headingPath: null });
  });

  it("returns empty array for empty or whitespace-only body", () => {
    expect(chunkMarkdown("")).toEqual([]);
    expect(chunkMarkdown("   \n\n  \n")).toEqual([]);
  });

  it("builds full ancestor chain in headingPath", () => {
    const body = [
      "preamble before any heading",
      "# Install",
      "intro text",
      "## Setup",
      "setup text",
      "### Advanced",
      "advanced text",
      "## Config",
      "config text",
    ].join("\n");
    const chunks = chunkMarkdown(body);
    expect(chunks.map((c) => c.headingPath)).toEqual([
      null,
      "Install",
      "Install > Setup",
      "Install > Setup > Advanced",
      "Install > Config",
    ]);
    expect(chunks.map((c) => c.ordinal)).toEqual([0, 1, 2, 3, 4]);
  });

  it("section content runs until next heading of any level", () => {
    const body = ["# A", "line one", "line two", "## B", "line three"].join(
      "\n",
    );
    const chunks = chunkMarkdown(body);
    const a = chunks.find((c) => c.headingPath === "A");
    expect(a?.content).toBe("line one\nline two");
    const b = chunks.find((c) => c.headingPath === "A > B");
    expect(b?.content).toBe("line three");
  });

  it("merges consecutive small chunks with same headingPath into previous", () => {
    const body = [
      "# Topic",
      repeatLine("filler sentence for the big section body.", 40), // > minChars
      "## Sub A",
      "tiny a",
      "tiny b",
      "## Sub B",
      "sub b body that is long enough to stand on its own without merging.",
    ].join("\n");
    const chunks = chunkMarkdown(body, { minChars: 200 });
    const paths = chunks.map((c) => c.headingPath);
    // "tiny a" merges into the Topic section chunk (same heading path? no — Topic vs Topic > Sub A differ)
    // Sub A's tiny chunks merge with each other only when adjacent with same path.
    expect(paths).toContain("Topic > Sub A");
    const subA = chunks.filter((c) => c.headingPath === "Topic > Sub A");
    expect(subA).toHaveLength(1);
    expect(subA[0]?.content).toContain("tiny a");
    expect(subA[0]?.content).toContain("tiny b");
  });

  it("keeps small chunk separate when merge would exceed maxChars*1.5", () => {
    const maxChars = 300;
    const big = "x".repeat(280);
    const small = "y".repeat(250);
    const body = `# A\n${big}\n## B\n${small}`;
    const chunks = chunkMarkdown(body, { maxChars, minChars: 200 });
    const a = chunks.find((c) => c.headingPath === "A");
    const b = chunks.find((c) => c.headingPath === "A > B");
    expect(a?.content.length).toBe(280);
    expect(b?.content.length).toBe(250);
  });

  it("splits oversized sections by paragraphs, lines, then hard slices", () => {
    const para = repeatLine("word word word", 10); // ~145 chars
    const body = `# Big\n\n${para}\n\n${para}\n\n${para}\n\n${para}\n\n${para}\n\n${para}`;
    const maxChars = 300;
    const chunks = chunkMarkdown(body, { maxChars });
    const bigChunks = chunks.filter((c) => c.headingPath === "Big");
    expect(bigChunks.length).toBeGreaterThan(1);
    for (const chunk of bigChunks) {
      expect(chunk.content.length).toBeLessThanOrEqual(maxChars);
    }
    // ordinals stay sequential across the whole document
    const first = bigChunks[0]?.ordinal ?? 0;
    bigChunks.forEach((chunk, i) => expect(chunk.ordinal).toBe(first + i));
  });

  it("hard-splits pathological single-token content respecting maxChars", () => {
    const blob = "z".repeat(1000);
    const body = `# Blob\n${blob}`;
    const chunks = chunkMarkdown(body, { maxChars: 300 });
    const blobChunks = chunks.filter((c) => c.headingPath === "Blob");
    expect(blobChunks.length).toBe(4);
    for (const chunk of blobChunks) {
      expect(chunk.content.length).toBeLessThanOrEqual(300);
      expect(chunk.content).toMatch(/^z+$/);
    }
    expect(blobChunks.map((c) => c.ordinal)).toEqual([0, 1, 2, 3]);
  });
});

describe("detectLanguage", () => {
  it("maps known extensions to language names", () => {
    expect(detectLanguage("main.ts")).toBe("typescript");
    expect(detectLanguage("app.tsx")).toBe("tsx");
    expect(detectLanguage("server.js")).toBe("javascript");
    expect(detectLanguage("cli.py")).toBe("python");
    expect(detectLanguage("lib.rs")).toBe("rust");
    expect(detectLanguage("main.go")).toBe("go");
    expect(detectLanguage("util.hpp")).toBe("cpp");
    expect(detectLanguage("a/b/c/config.yml")).toBe("yaml");
    expect(detectLanguage("style.css")).toBe("css");
  });

  it("is case-insensitive on the extension", () => {
    expect(detectLanguage("README.MD")).toBe("markdown");
  });

  it("returns null for unknown or missing extensions", () => {
    expect(detectLanguage("artifact.bin")).toBeNull();
    expect(detectLanguage("Makefile")).toBeNull();
    expect(detectLanguage("")).toBeNull();
  });
});

describe("chunkCode", () => {
  it("returns null-path chunks when no symbols are detected", () => {
    const source = repeatLine("plain text no symbols", 5);
    const chunks = chunkCode(source, "text");
    expect(chunks).toHaveLength(1);
    expect(chunks[0]?.headingPath).toBeNull();
    expect(chunks[0]?.ordinal).toBe(0);
  });

  it("builds symbol path for class methods in brace languages", () => {
    const source = [
      "export class CheckpointLoader {",
      "  private loadManifest() {",
      "    return 1;",
      "  }",
      "",
      "  save(path: string): void {",
      "    void path;",
      "  }",
      "}",
    ].join("\n");
    const chunks = chunkCode(source, "typescript");
    const paths = chunks.map((c) => c.headingPath);
    expect(paths).toContain("CheckpointLoader");
    expect(paths).toContain("CheckpointLoader > loadManifest()");
    expect(paths).toContain("CheckpointLoader > save()");
  });

  it("handles python def/class with indentation nesting", () => {
    const source = [
      "class Trainer:",
      "    def fit(self):",
      "        pass",
      "",
      "    def predict(self):",
      "        pass",
    ].join("\n");
    const chunks = chunkCode(source, "python");
    const paths = chunks.map((c) => c.headingPath);
    expect(paths).toContain("Trainer");
    expect(paths).toContain("Trainer > fit()");
    expect(paths).toContain("Trainer > predict()");
  });

  it("captures top-level functions and preamble separately", () => {
    const source = [
      "import os",
      "",
      "def main():",
      "    pass",
      "",
      "def helper():",
      "    pass",
    ].join("\n");
    const chunks = chunkCode(source, "python");
    const preamble = chunks.find((c) => c.headingPath === null);
    expect(preamble?.content).toContain("import os");
    expect(chunks.map((c) => c.headingPath)).toEqual([
      null,
      "main()",
      "helper()",
    ]);
  });

  it("falls back to blank-line groups when a single declaration exceeds maxChars", () => {
    const group = repeatLine("statement();", 8); // ~100 chars
    const methodBody = [group, "", group, "", group, "", group, "", group].join(
      "\n",
    );
    const source = ["class Big {", "  run() {", methodBody, "  }", "}"].join(
      "\n",
    );
    const maxChars = 250;
    const chunks = chunkCode(source, "typescript", { maxChars });
    const runChunks = chunks.filter((c) => c.headingPath === "Big > run()");
    expect(runChunks.length).toBeGreaterThan(1);
    for (const chunk of runChunks) {
      expect(chunk.content.length).toBeLessThanOrEqual(maxChars);
    }
  });

  it("hard-splits pathological code respecting maxChars", () => {
    const source = `function blob() { ${"x".repeat(900)} }`;
    const chunks = chunkCode(source, "javascript", { maxChars: 300 });
    expect(chunks.length).toBeGreaterThanOrEqual(3);
    for (const chunk of chunks) {
      expect(chunk.content.length).toBeLessThanOrEqual(300);
    }
    expect(chunks.every((c) => c.headingPath === "blob()")).toBe(true);
  });

  it("keeps ordinals sequential and deterministic across runs", () => {
    const source = [
      "class A {",
      "  one() { return 1; }",
      "  two() { return 2; }",
      "}",
      "class B {",
      "  three() { return 3; }",
      "}",
    ].join("\n");
    const first = chunkCode(source, "typescript");
    const second = chunkCode(source, "typescript");
    expect(first).toEqual(second);
    first.forEach((chunk, i) => expect(chunk.ordinal).toBe(i));
  });

  it("splits oversized top-level declarations into sub-chunks sharing headingPath", () => {
    const stmt = repeatLine("doSomething(withArgs);", 6); // ~140 chars
    const fnBody = [stmt, stmt, stmt, stmt].join("\n\n");
    const source = ["fn compute() {", fnBody, "}"].join("\n");
    const chunks = chunkCode(source, "rust", { maxChars: 300 });
    const computeChunks = chunks.filter((c) => c.headingPath === "compute()");
    expect(computeChunks.length).toBeGreaterThan(1);
    computeChunks.forEach((chunk, i) => {
      expect(chunk.content.length).toBeLessThanOrEqual(300);
      if (i > 0) {
        const prev = computeChunks[i - 1];
        expect(chunk.ordinal).toBe((prev?.ordinal ?? -1) + 1);
      }
    });
  });
});

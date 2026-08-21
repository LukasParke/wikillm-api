import { describe, expect, it } from "vitest";
import {
  extractLinks,
  extractWikilinks,
  parseMarkdownDocument,
  resolveLinkTarget,
} from "../../src/okf/parse.js";
import {
  actorFromSource,
  deriveTrustTier,
  isStale,
  normalizeVerified,
} from "../../src/okf/trust.js";
import { validateBundle, validateConceptFile } from "../../src/okf/validate.js";

const HAPPY_CONCEPT = `---
type: entity
title: OpenAI
verified:
  - by: "human:luke"
    at: "2026-01-15T10:00:00Z"
stale_after: "2027-01-01T00:00:00Z"
---

# OpenAI

See [[GPT-4]] and the [spec](../spec/okf.md#overview).
Also an image: ![logo](assets/logo.png) and [site](https://openai.com).
`;

describe("extractLinks", () => {
  it("extracts link and image targets, skipping external", () => {
    const body = [
      "[text](target.md)",
      "![alt](img.png)",
      "[ext](https://example.com/x)",
      "[mail](mailto:a@b.c)",
      "[anchor](#section)",
    ].join("\n");
    expect(extractLinks(body)).toEqual(["target.md", "img.png"]);
  });

  it("strips anchor suffixes and dedupes preserving order", () => {
    const body =
      "[a](docs/guide.md#intro)\n[b](docs/guide.md#advanced)\n[c](docs/guide.md)";
    expect(extractLinks(body)).toEqual(["docs/guide.md"]);
  });

  it("handles titles and angle-bracket targets", () => {
    const body = '[t](file%20name.md "Title")\n[u](<angled path.md>)';
    expect(extractLinks(body)).toEqual(["file%20name.md", "angled path.md"]);
  });

  it("returns empty for no links", () => {
    expect(extractLinks("just plain text\nwith no links")).toEqual([]);
  });
});

describe("extractWikilinks", () => {
  it("extracts plain and aliased wikilinks, deduped in order", () => {
    const body = "See [[Foo]] then [[Bar|the bar page]] then [[Foo]] again.";
    expect(extractWikilinks(body)).toEqual(["Foo", "Bar"]);
  });

  it("ignores markdown links and empty targets", () => {
    const body = "[not](a wikilink.md) [[ ]] [[]]";
    expect(extractWikilinks(body)).toEqual([]);
  });
});

describe("resolveLinkTarget", () => {
  const cases: Array<[string, string, string | null]> = [
    ["Other.md", "concepts/a.md", "concepts/Other.md"],
    ["Other", "concepts/a.md", "concepts/Other.md"],
    ["/entities/OpenAI.md", "notes/deep/page.md", "entities/OpenAI.md"],
    ["./sibling.md", "a/b/c.md", "a/b/sibling.md"],
    ["../parent.md", "a/b/c.md", "a/parent.md"],
    ["../../escape.md", "a/b/c.md", "escape.md"],
    ["../../../escape.md", "a/b/c.md", null],
    ["https://example.com/x", "a.md", null],
    ["mailto:x@y.z", "a.md", null],
    ["#section-only", "a.md", null],
    ["page.md#section", "concepts/a.md", "concepts/page.md"],
    ["image.png", "concepts/a.md", "concepts/image.png"],
  ];
  for (const [link, source, expected] of cases) {
    it(`resolves ${JSON.stringify(link)} from ${source}`, () => {
      expect(resolveLinkTarget(link, source)).toBe(expected);
    });
  }
});

describe("parseMarkdownDocument", () => {
  it("parses a happy-path concept document", () => {
    const doc = parseMarkdownDocument(HAPPY_CONCEPT);
    expect(doc.frontmatter.type).toBe("entity");
    expect(doc.body).toContain("# OpenAI");
    expect(doc.wikilinks).toEqual(["GPT-4"]);
    expect(doc.links).toEqual(["../spec/okf.md", "assets/logo.png"]);
  });
});

describe("deriveTrustTier / normalizeVerified", () => {
  it("returns unverified for absent, null, and empty array", () => {
    expect(deriveTrustTier(undefined)).toBe("unverified");
    expect(deriveTrustTier(null)).toBe("unverified");
    expect(deriveTrustTier([])).toBe("unverified");
    expect(normalizeVerified(undefined)).toBeNull();
    expect(normalizeVerified([])).toBeNull();
  });

  it("classifies machine-confirmed and human-reviewed", () => {
    expect(deriveTrustTier([{ by: "crawler/wikillm-api" }])).toBe(
      "machine-confirmed",
    );
    expect(deriveTrustTier([{ by: "human:luke" }])).toBe("human-reviewed");
    expect(deriveTrustTier([{ by: "bot" }, { by: "human:luke" }])).toBe(
      "human-reviewed",
    );
  });

  it("treats a bare {by, at} mapping as a one-element list (§5.2)", () => {
    expect(deriveTrustTier({ by: "human:luke", at: "2026-01-01" })).toBe(
      "human-reviewed",
    );
    expect(normalizeVerified({ by: "bot" })).toEqual([{ by: "bot", at: "" }]);
  });

  it("drops entries missing by and ignores non-object entries", () => {
    expect(normalizeVerified([{ at: "2026-01-01" }, "junk", 42])).toBeNull();
    expect(normalizeVerified([{ at: "x" }, { by: "bot" }])).toEqual([
      { by: "bot", at: "" },
    ]);
  });
});

describe("isStale", () => {
  const now = new Date("2026-06-01T00:00:00Z");

  it("flags past dates and not future dates", () => {
    expect(isStale("2026-01-01T00:00:00Z", now)).toBe(true);
    expect(isStale("2027-01-01T00:00:00Z", now)).toBe(false);
  });

  it("returns false for absent or invalid values", () => {
    expect(isStale(undefined, now)).toBe(false);
    expect(isStale(null, now)).toBe(false);
    expect(isStale("", now)).toBe(false);
    expect(isStale("not-a-date", now)).toBe(false);
    expect(isStale(12345, now)).toBe(false);
  });

  it("accepts Date instances from YAML timestamp parsing", () => {
    expect(isStale(new Date("2025-12-31T00:00:00Z"), now)).toBe(true);
  });
});

describe("actorFromSource", () => {
  it("maps listed human actors case-insensitively", () => {
    expect(actorFromSource("Luke", ["luke"])).toBe("human:Luke");
  });

  it("maps user-/human- prefixed sources to human actors", () => {
    expect(actorFromSource("user-alice")).toBe("human:user-alice");
    expect(actorFromSource("Human-Bob")).toBe("human:Human-Bob");
  });

  it("namespaces machine sources under wikillm-api (§7)", () => {
    expect(actorFromSource("web-crawler")).toBe("web-crawler/wikillm-api");
  });
});

describe("validateConceptFile", () => {
  it("accepts a happy-path concept without issues", () => {
    expect(validateConceptFile("entities/openai.md", HAPPY_CONCEPT)).toEqual(
      [],
    );
  });

  it("errors on missing or empty type", () => {
    const noType = "---\ntitle: x\n---\nbody";
    const emptyType = '---\ntype: ""\n---\nbody';
    expect(validateConceptFile("a.md", noType)).toHaveLength(1);
    expect(validateConceptFile("a.md", noType)[0].level).toBe("error");
    expect(validateConceptFile("a.md", emptyType)[0].level).toBe("error");
    expect(validateConceptFile("a.md", "no frontmatter at all")[0].level).toBe(
      "error",
    );
  });

  it("errors on unparseable YAML frontmatter", () => {
    const bad = '---\ntype: "unterminated\n---\nbody';
    const issues = validateConceptFile("a.md", bad);
    expect(
      issues.some((i) => i.level === "error" && /frontmatter/.test(i.message)),
    ).toBe(true);
  });

  it("exempts reserved index.md/log.md from the type requirement", () => {
    expect(validateConceptFile("index.md", "# Home\n")).toEqual([]);
    expect(validateConceptFile("notes/index.md", "# Notes\n")).toEqual([]);
    expect(validateConceptFile("log.md", "# Log\n")).toEqual([]);
    expect(validateConceptFile("deep/dir/log.md", "")).toEqual([]);
  });

  it("warns on malformed log.md date headings", () => {
    const raw = "# Log\n\n## 2026-01-01\n\n## January 2nd\n\n## 2026-1-3\n";
    const issues = validateConceptFile("log.md", raw);
    expect(issues).toHaveLength(2);
    expect(issues.every((i) => i.level === "warning")).toBe(true);
  });

  it("warns when root index.md carries a non-string okf_version", () => {
    const raw = "---\nokf_version: 0.2\n---\n# Home";
    const rootIssues = validateConceptFile("index.md", raw);
    expect(rootIssues).toHaveLength(1);
    expect(rootIssues[0].level).toBe("warning");
    // nested index.md must not be checked for okf_version
    expect(validateConceptFile("sub/index.md", raw)).toEqual([]);
  });
});

describe("validateBundle", () => {
  it("aggregates errors, warnings, and stats over concept files only", () => {
    const report = validateBundle([
      { relPath: "index.md", content: '---\nokf_version: "0.2"\n---\n# Home' },
      { relPath: "log.md", content: "# Log\n\n## bad heading\n" },
      {
        relPath: "entities/openai.md",
        content: HAPPY_CONCEPT,
      },
      {
        relPath: "entities/anthropic.md",
        content:
          '---\ntype: entity\nverified:\n  - by: human:luke\nstale_after: "2020-01-01T00:00:00Z"\n---\nbody',
      },
      {
        relPath: "notes/snippet.md",
        content: "---\ntype: note\n---\nbody",
      },
      { relPath: "broken.md", content: "no frontmatter" },
    ]);

    expect(report.valid).toBe(false);
    expect(report.errors).toHaveLength(1);
    expect(report.errors[0].path).toBe("broken.md");
    expect(report.warnings).toHaveLength(1);

    expect(report.stats.concepts).toBe(3);
    expect(report.stats.byType).toEqual({ entity: 2, note: 1 });
    expect(report.stats.trustTiers).toEqual({
      "human-reviewed": 2,
      unverified: 1,
    });
    expect(report.stats.staleCount).toBe(1);
  });

  it("reports valid=true for a clean bundle", () => {
    const report = validateBundle([
      { relPath: "a.md", content: "---\ntype: t\n---\nx" },
    ]);
    expect(report.valid).toBe(true);
    expect(report.errors).toEqual([]);
  });
});

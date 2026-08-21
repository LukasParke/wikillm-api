import { describe, it, expect } from "vitest";
import { rrfFuse, recencyBoost } from "../../src/services/searchService.js";

describe("reciprocal rank fusion", () => {
  it("rewards consensus across lists over a single top rank", () => {
    const fused = rrfFuse([
      ["a", "b", "c"],
      ["b", "a", "d"],
    ]);
    const scores = new Map(fused.map(({ key, score }) => [key, score]));
    // a and b appear in both lists; b ranks 1st once and 2nd once,
    // a ranks 1st once and 2nd once -> tie; both must beat c and d.
    expect(scores.get("a")!).toBeCloseTo(scores.get("b")!, 10);
    expect(scores.get("a")!).toBeGreaterThan(scores.get("c")!);
    expect(scores.get("b")!).toBeGreaterThan(scores.get("d")!);
  });

  it("uses the standard k=60 smoothing", () => {
    const fused = rrfFuse([["x"]], 60);
    expect(fused[0].score).toBeCloseTo(1 / 61, 10);
  });

  it("deduplicates keys within the union", () => {
    const fused = rrfFuse([["a"], ["a"]]);
    expect(fused).toHaveLength(1);
    expect(fused[0].score).toBeCloseTo(2 / 61, 10);
  });

  it("handles empty input", () => {
    expect(rrfFuse([])).toEqual([]);
  });
});

describe("recency boost", () => {
  it("is 1 for fresh content and decays exponentially", () => {
    const now = Date.now();
    expect(recencyBoost(now, now)).toBe(1);
    const thirtyDays = now - 30 * 86_400_000;
    expect(recencyBoost(thirtyDays, now)).toBeCloseTo(Math.exp(-1), 6);
    expect(recencyBoost(now - 365 * 86_400_000, now)).toBeLessThan(0.001);
  });

  it("never amplifies future-dated content beyond 1", () => {
    const now = Date.now();
    expect(recencyBoost(now + 86_400_000, now)).toBe(1);
  });
});

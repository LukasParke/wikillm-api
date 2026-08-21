/**
 * Shared full-text query shaping for both store backends.
 * Escapes user input into safe FTS terms joined with OR (recall-first);
 * BM25/ts_rank provide the IDF-style weighting on top.
 */
export function ftsQuery(q: string): string {
  const terms = q
    .split(/\s+/)
    .map((t) => t.replace(/["'()*:^]/g, " ").trim())
    .filter((t) => t.length > 0)
    .slice(0, 12);
  if (terms.length === 0) return "";
  return terms.map((t) => `"${t}"`).join(" OR ");
}

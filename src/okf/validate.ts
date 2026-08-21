import matter from "gray-matter";
import { deriveTrustTier, isStale, normalizeVerified } from "./trust.js";

export interface ValidationIssue {
  level: "error" | "warning";
  path?: string;
  message: string;
}

export interface BundleValidationReport {
  valid: boolean;
  errors: ValidationIssue[];
  warnings: ValidationIssue[];
  stats: {
    concepts: number;
    byType: Record<string, number>;
    trustTiers: Record<string, number>;
    staleCount: number;
  };
}

const RESERVED_FILENAMES: Record<string, true> = {
  "index.md": true,
  "log.md": true,
};

const LOG_DATE_HEADING_RE = /^## \d{4}-\d{2}-\d{2}\s*$/;

interface FrontmatterResult {
  data: Record<string, unknown>;
  error: string | null;
}

function parseFrontmatter(raw: string): FrontmatterResult {
  let parsed: matter.GrayMatterFile<string>;
  try {
    parsed = matter(raw);
  } catch (cause) {
    const message = cause instanceof Error ? cause.message : String(cause);
    return { data: {}, error: `unparseable YAML frontmatter: ${message}` };
  }
  if (
    typeof parsed.data !== "object" ||
    parsed.data === null ||
    Array.isArray(parsed.data)
  ) {
    return { data: {}, error: "frontmatter is not a mapping" };
  }
  return { data: parsed.data, error: null };
}

function isConceptFrontmatter(
  data: Record<string, unknown>,
): data is Record<string, unknown> & { type: string } {
  return typeof data.type === "string" && data.type.trim() !== "";
}

/** Validate a single concept file per OKF v0.2. Reserved filenames
 * (`index.md`/`log.md` at any depth) are exempt from the frontmatter/type
 * requirement; log.md date headings and root index.md okf_version produce
 * warnings. Cross-file link validation is out of scope here. */
export function validateConceptFile(
  relPath: string,
  raw: string,
): ValidationIssue[] {
  const issues: ValidationIssue[] = [];
  const base = relPath.split("/").pop() ?? "";
  // Agent-instruction files (AGENTS.md/CLAUDE.md) are bundle configuration in
  // this service, not concepts; skip frontmatter checks for them.
  const lowered = base.toLowerCase();
  if (lowered === "agents.md" || lowered === "claude.md") {
    return issues;
  }
  const frontmatter = parseFrontmatter(raw);
  if (frontmatter.error !== null) {
    issues.push({
      level: "error",
      path: relPath,
      message: frontmatter.error,
    });
  }

  if (base === "log.md") {
    const lines = raw.split(/\r?\n/);
    lines.forEach((line, index) => {
      if (/^##(?!#)/.test(line) && !LOG_DATE_HEADING_RE.test(line)) {
        issues.push({
          level: "warning",
          path: `${relPath}#L${index + 1}`,
          message: `log heading must match "## YYYY-MM-DD", got ${JSON.stringify(line)}`,
        });
      }
    });
  } else if (base === "index.md") {
    if (relPath === "index.md") {
      const okfVersion = frontmatter.data.okf_version;
      if (okfVersion !== undefined && typeof okfVersion !== "string") {
        issues.push({
          level: "warning",
          path: relPath,
          message: "okf_version must be a string when present",
        });
      }
    }
  } else if (!isConceptFrontmatter(frontmatter.data)) {
    issues.push({
      level: "error",
      path: relPath,
      message: "missing or empty 'type' in frontmatter",
    });
  }
  return issues;
}

export function validateBundle(
  files: Array<{ relPath: string; content: string }>,
): BundleValidationReport {
  const errors: ValidationIssue[] = [];
  const warnings: ValidationIssue[] = [];
  const byType: Record<string, number> = {};
  const trustTiers: Record<string, number> = {};
  let concepts = 0;
  let staleCount = 0;

  for (const file of files) {
    for (const issue of validateConceptFile(file.relPath, file.content)) {
      (issue.level === "error" ? errors : warnings).push(issue);
    }
    const base = file.relPath.split("/").pop() ?? "";
    if (RESERVED_FILENAMES[base] === true) continue;
    const frontmatter = parseFrontmatter(file.content);
    if (frontmatter.error !== null) continue;
    if (!isConceptFrontmatter(frontmatter.data)) continue;
    concepts++;
    const { type } = frontmatter.data;
    byType[type] = (byType[type] ?? 0) + 1;
    const tier = deriveTrustTier(normalizeVerified(frontmatter.data.verified));
    trustTiers[tier] = (trustTiers[tier] ?? 0) + 1;
    if (isStale(frontmatter.data.stale_after)) staleCount++;
  }

  return {
    valid: errors.length === 0,
    errors,
    warnings,
    stats: { concepts, byType, trustTiers, staleCount },
  };
}

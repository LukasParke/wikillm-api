import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { isIgnoredPath, relativeToWiki } from "../fs/paths.js";
import { validateBundle, validateConceptFile } from "../okf/validate.js";
import type {
  BundleValidationReport,
  ValidationIssue,
} from "../okf/validate.js";
import type { Config } from "../config.js";

export class OkfService {
  constructor(private readonly config: Config) {}

  validateSingle(raw: string, relPath = "concept.md"): ValidationIssue[] {
    return validateConceptFile(relPath, raw);
  }

  async validateWikiBundle(): Promise<BundleValidationReport> {
    const files = this.collectMarkdown();
    return validateBundle(files);
  }

  collectMarkdown(): Array<{ relPath: string; content: string }> {
    const out: Array<{ relPath: string; content: string }> = [];
    const visit = (dir: string) => {
      let entries;
      try {
        entries = readdirSync(dir, { withFileTypes: true });
      } catch {
        return;
      }
      for (const entry of entries) {
        const full = path.join(dir, entry.name);
        const rel = relativeToWiki(this.config.WIKI_ROOT, full);
        if (isIgnoredPath(rel)) continue;
        if (entry.isDirectory()) visit(full);
        else if (entry.isFile() && entry.name.endsWith(".md")) {
          try {
            out.push({ relPath: rel, content: readFileSync(full, "utf8") });
          } catch {
            // unreadable file: skip
          }
        }
      }
    };
    visit(this.config.WIKI_ROOT);
    return out;
  }

  layoutProfile(): "okf" | "wikillm" {
    if (this.config.LAYOUT !== "auto") return this.config.LAYOUT;
    const rootIndex = path.join(this.config.WIKI_ROOT, "index.md");
    if (exists(rootIndex)) {
      const content = readFileSync(rootIndex, "utf8");
      if (content.includes("okf_version")) return "okf";
    }
    return exists(path.join(this.config.WIKI_ROOT, "wiki")) ? "wikillm" : "okf";
  }
}

function exists(p: string): boolean {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

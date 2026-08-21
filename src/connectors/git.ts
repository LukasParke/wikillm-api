import { execFile as execFileCb } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { isIgnoredPath } from "../fs/paths.js";
import type { ConnectorImpl, ConnectorPollResult } from "./types.js";
import { strConfig, strListConfig } from "./types.js";

const execFile = promisify(execFileCb);

interface GitState {
  commit?: string;
}

/**
 * Git repository connector: shallow-clones once into a temp cache, then
 * fetch/reset on each poll. Emits every matched file; the index pipeline
 * dedupes unchanged documents by content hash.
 */
export const gitConnector: ConnectorImpl = {
  kind: "git",
  async poll(config, state, ctx): Promise<ConnectorPollResult> {
    const url = strConfig(config, "url");
    if (!url) throw new Error("git connector requires config.url");
    const branch = strConfig(config, "branch");
    const extensions = strListConfig(config, "extensions", [".md"]);
    const dir = cacheDir(url);

    await ensureRepo(dir, url, branch);
    const commit = (await git(dir, ["rev-parse", "HEAD"])).trim();
    const previous = (state ?? {}) as GitState;
    if (previous.commit === commit) {
      return { docs: [], state };
    }

    const docs = [];
    for (const filePath of walk(dir, dir)) {
      const ext = path.extname(filePath).toLowerCase();
      if (!extensions.includes(ext)) continue;
      const rel = path.relative(dir, filePath).replace(/\\/g, "/");
      try {
        const stat = statSync(filePath);
        docs.push({
          path: rel,
          content: readFileSync(filePath, "utf8"),
          mtime: Math.floor(stat.mtimeMs),
        });
      } catch (err) {
        ctx.log(`git connector read failed: ${rel}`, err);
      }
    }
    ctx.log(
      `git connector ${url}: ${docs.length} files at ${commit.slice(0, 12)}`,
    );
    return { docs, state: { commit } satisfies GitState };
  },
};

function cacheDir(url: string): string {
  const hash = createHash("sha256").update(url).digest("hex").slice(0, 16);
  return path.join(tmpdir(), `wikillm-git-${hash}`);
}

async function ensureRepo(
  dir: string,
  url: string,
  branch?: string,
): Promise<void> {
  if (existsSync(path.join(dir, ".git"))) {
    if (branch) {
      await git(dir, ["fetch", "--depth", "1", "origin", branch]);
      await git(dir, ["reset", "--hard", "FETCH_HEAD"]);
    } else {
      await git(dir, ["fetch", "--depth", "1", "origin", "HEAD"]);
      await git(dir, ["reset", "--hard", "FETCH_HEAD"]);
    }
    return;
  }
  const args = ["clone", "--depth", "1"];
  if (branch) args.push("--branch", branch);
  args.push(url, dir);
  await git(".", args);
}

async function git(cwd: string, args: string[]): Promise<string> {
  const { stdout } = await execFile("git", args, {
    cwd,
    maxBuffer: 32 * 1024 * 1024,
  });
  return stdout;
}

function walk(root: string, dir: string): string[] {
  const out: string[] = [];
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    const rel = path.relative(root, full).replace(/\\/g, "/");
    if (isIgnoredPath(rel) || entry.name === ".git") continue;
    if (entry.isDirectory()) out.push(...walk(root, full));
    else if (entry.isFile()) out.push(full);
  }
  return out;
}

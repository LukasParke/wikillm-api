export type TrustTier = "unverified" | "machine-confirmed" | "human-reviewed";

export interface VerifiedEntry {
  by: string;
  at: string;
}

interface RawEntry {
  [key: string]: unknown;
}

function asRecord(value: unknown): RawEntry | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  return value as RawEntry;
}

/** Accepts a list of verification entries or a bare `{by, at}` mapping
 * (spec §5.2); returns normalized entries or null for absent/empty. */
export function normalizeVerified(verified: unknown): VerifiedEntry[] | null {
  let rawEntries: unknown[];
  if (verified === undefined || verified === null) return null;
  if (Array.isArray(verified)) {
    rawEntries = verified;
  } else {
    rawEntries = [verified];
  }
  const out: VerifiedEntry[] = [];
  for (const raw of rawEntries) {
    const record = asRecord(raw);
    if (record === null) continue;
    const by = record.by;
    if (typeof by !== "string" || by === "") continue;
    const at = typeof record.at === "string" ? record.at : "";
    out.push({ by, at });
  }
  return out.length > 0 ? out : null;
}

export function deriveTrustTier(verified: unknown): TrustTier {
  const entries = normalizeVerified(verified);
  if (entries === null) return "unverified";
  if (entries.some((entry) => entry.by.startsWith("human:"))) {
    return "human-reviewed";
  }
  return "machine-confirmed";
}

export function isStale(staleAfter: unknown, now: Date = new Date()): boolean {
  let timestamp: number;
  if (staleAfter instanceof Date) {
    timestamp = staleAfter.getTime();
  } else if (typeof staleAfter === "string" && staleAfter !== "") {
    timestamp = Date.parse(staleAfter);
  } else {
    return false;
  }
  if (Number.isNaN(timestamp)) return false;
  return timestamp <= now.getTime();
}

/** Spec §7 actor convention: human actors get `human:<name>`; machine sources
 * get `<source>/wikillm-api`. */
export function actorFromSource(
  sourceName: string,
  humanActors?: Iterable<string>,
): string {
  const lowered = sourceName.toLowerCase();
  if (humanActors !== undefined) {
    for (const actor of humanActors) {
      if (actor.toLowerCase() === lowered) return `human:${sourceName}`;
    }
  }
  if (lowered.startsWith("user-") || lowered.startsWith("human-")) {
    return `human:${sourceName}`;
  }
  return `${sourceName}/wikillm-api`;
}

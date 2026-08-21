import type { Store } from "../store/types.js";

export interface GraphView {
  nodes: Array<{ rel_path: string; title: string | null; exists: boolean }>;
  edges: Array<{ src: string; dst: string }>;
}

export class GraphService {
  constructor(private readonly store: Store) {}

  async neighbors(relPath: string, depth = 1): Promise<GraphView> {
    const nodes = new Map<string, { title: string | null; exists: boolean }>();
    const edges = new Map<string, { src: string; dst: string }>();
    const frontier = new Set([relPath]);
    const visited = new Set<string>(frontier);

    for (let level = 0; level < depth; level += 1) {
      const next = new Set<string>();
      const expansions = await Promise.all(
        [...frontier].map(async (current) => {
          const doc = await this.store.getDocument(current);
          const outgoingRaw = doc?.outgoing_links ?? [];
          // Links are stored pre-resolved as bundle-absolute "/x/y.md".
          const outgoing = outgoingRaw
            .map((link) => link.replace(/^\//, ""))
            .filter((t) => t.length > 0);
          const incoming = await this.store.backlinks(current);
          return { current, outgoing, incoming };
        }),
      );
      for (const { current, outgoing, incoming } of expansions) {
        for (const target of outgoing) {
          edges.set(`${current}->${target}`, { src: current, dst: target });
          if (!visited.has(target)) next.add(target);
        }
        for (const source of incoming) {
          edges.set(`${source}->${current}`, { src: source, dst: current });
          if (!visited.has(source)) next.add(source);
        }
      }
      for (const n of next) visited.add(n);
      frontier.clear();
      for (const n of next) frontier.add(n);
      if (frontier.size === 0) break;
    }

    // Resolve node metadata; missing targets stay as not-yet-written concepts.
    const ids = [relPath, ...edgesToNodeIds(edges)].filter(
      (id) => !nodes.has(id),
    );
    await Promise.all(
      ids.map(async (id) => {
        if (nodes.has(id)) return;
        const doc = await this.store.getDocument(id);
        nodes.set(id, { title: doc?.title ?? null, exists: doc !== null });
      }),
    );

    return {
      nodes: [...nodes.entries()].map(([rel_path, meta]) => ({
        rel_path,
        ...meta,
      })),
      edges: [...edges.values()],
    };
  }
}

function edgesToNodeIds(
  edges: Map<string, { src: string; dst: string }>,
): string[] {
  const out = new Set<string>();
  for (const edge of edges.values()) {
    out.add(edge.src);
    out.add(edge.dst);
  }
  return [...out];
}

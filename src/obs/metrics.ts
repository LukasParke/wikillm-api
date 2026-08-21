type LabelSet = Record<string, string>;

interface CounterState {
  help: string;
  values: Map<string, number>; // labelKey -> value
}

interface HistogramState {
  help: string;
  buckets: number[];
  counts: Map<string, number[]>; // labelKey -> per-bucket counts
  sums: Map<string, number>;
  totals: Map<string, number>;
}

const DEFAULT_BUCKETS = [
  0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10,
];

/**
 * Minimal Prometheus text-exposition registry. Deliberately dependency-free:
 * the metric surface is small and stable.
 */
export class MetricsRegistry {
  private counters = new Map<string, CounterState>();
  private histograms = new Map<string, HistogramState>();

  counter(name: string, help: string, labels: LabelSet = {}, by = 1): void {
    let state = this.counters.get(name);
    if (!state) {
      state = { help, values: new Map() };
      this.counters.set(name, state);
    }
    const key = labelKey(labels);
    state.values.set(key, (state.values.get(key) ?? 0) + by);
  }

  observe(
    name: string,
    help: string,
    value: number,
    labels: LabelSet = {},
    buckets: number[] = DEFAULT_BUCKETS,
  ): void {
    let state = this.histograms.get(name);
    if (!state) {
      state = {
        help,
        buckets,
        counts: new Map(),
        sums: new Map(),
        totals: new Map(),
      };
      this.histograms.set(name, state);
    }
    const key = labelKey(labels);
    if (!state.counts.has(key)) {
      state.counts.set(
        key,
        state.buckets.map(() => 0),
      );
      state.sums.set(key, 0);
      state.totals.set(key, 0);
    }
    const counts = state.counts.get(key)!;
    for (let i = 0; i < state.buckets.length; i += 1) {
      if (value <= state.buckets[i]) counts[i] += 1;
    }
    state.sums.set(key, (state.sums.get(key) ?? 0) + value);
    state.totals.set(key, (state.totals.get(key) ?? 0) + 1);
  }

  render(): string {
    const lines: string[] = [];
    for (const [name, state] of this.counters) {
      lines.push(`# HELP ${name} ${state.help}`);
      lines.push(`# TYPE ${name} counter`);
      for (const [key, value] of state.values) {
        lines.push(`${name}${key} ${value}`);
      }
      if (state.values.size === 0) lines.push(`${name} 0`);
    }
    for (const [name, state] of this.histograms) {
      lines.push(`# HELP ${name} ${state.help}`);
      lines.push(`# TYPE ${name} histogram`);
      for (const [key, counts] of state.counts) {
        for (let i = 0; i < state.buckets.length; i += 1) {
          lines.push(
            `${name}_bucket${mergeLabels(key, state.buckets[i])} ${counts[i]}`,
          );
        }
        lines.push(
          `${name}_bucket${mergeLabels(key, "+Inf")} ${state.totals.get(key) ?? 0}`,
        );
        lines.push(`${name}_sum${key} ${round(state.sums.get(key) ?? 0)}`);
        lines.push(`${name}_count${key} ${state.totals.get(key) ?? 0}`);
      }
    }
    return `${lines.join("\n")}\n`;
  }

  reset(): void {
    this.counters.clear();
    this.histograms.clear();
  }
}

function labelKey(labels: LabelSet): string {
  const parts = Object.entries(labels).sort(([a], [b]) => a.localeCompare(b));
  if (parts.length === 0) return "";
  return `{${parts.map(([k, v]) => `${k}="${escapeLabel(v)}"`).join(",")}}`;
}

function mergeLabels(existingKey: string, bucket: number | string): string {
  const inner = existingKey.startsWith("{")
    ? existingKey.slice(1, -1)
    : existingKey;
  return inner ? `{${inner},le="${bucket}"}` : `{le="${bucket}"}`;
}

function escapeLabel(value: string): string {
  return value
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n");
}

function round(value: number): number {
  return Number.isFinite(value) ? Number(value.toFixed(6)) : 0;
}

export const metrics = new MetricsRegistry();

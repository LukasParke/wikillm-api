//! Minimal Prometheus text-exposition registry (dependency-free).

use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MetricsRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    counters: BTreeMap<String, f64>,
    histograms: BTreeMap<String, HistState>,
}

#[derive(Default)]
struct HistState {
    help_set: bool,
    buckets: Vec<f64>,
    counts: BTreeMap<String, Vec<u64>>,
    sums: BTreeMap<String, f64>,
    totals: BTreeMap<String, u64>,
}

const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

fn label_key(labels: &[(&str, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")))
        .collect();
    parts.sort();
    format!("{{{}}}", parts.join(","))
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counter(&self, name: &str, labels: &[(&str, String)], by: f64) {
        let mut inner = self.inner.lock().unwrap();
        *inner.counters.entry(format!("{name}{}", label_key(labels))).or_insert(0.0) += by;
    }

    pub fn observe(&self, name: &str, value: f64, labels: &[(&str, String)]) {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .histograms
            .entry(name.to_string())
            .or_insert_with(|| HistState { buckets: DEFAULT_BUCKETS.to_vec(), ..Default::default() });
        let key = label_key(labels);
        let entry = state
            .counts
            .entry(key.clone())
            .or_insert_with(|| vec![0; state.buckets.len()]);
        for (i, b) in state.buckets.iter().enumerate() {
            if value <= *b {
                entry[i] += 1;
            }
        }
        *state.sums.entry(key.clone()).or_default() += value;
        *state.totals.entry(key).or_default() += 1;
    }

    pub fn render(&self) -> String {
        let inner = self.inner.lock().unwrap();
        let mut out = String::new();
        for (name_with_labels, value) in &inner.counters {
            let base = name_with_labels.split('{').next().unwrap_or(name_with_labels);
            out.push_str(&format!("# TYPE {base} counter\n"));
            out.push_str(&format!("{name_with_labels} {value}\n"));
        }
        for (name, state) in &inner.histograms {
            out.push_str(&format!("# TYPE {name} histogram\n"));
            for (key, counts) in &state.counts {
                for (i, b) in state.buckets.iter().enumerate() {
                    let le = format!("{b}");
                    let merged = if key.is_empty() {
                        format!("{{le=\"{le}\"}}")
                    } else {
                        format!("{}{},le=\"{le}\"}}", &key[..key.len() - 1], ",")
                    };
                    out.push_str(&format!("{name}_bucket{merged} {}\n", counts[i]));
                }
                let total = state.totals.get(key).copied().unwrap_or(0);
                let merged_inf = if key.is_empty() {
                    "{le=\"+Inf\"}".to_string()
                } else {
                    format!("{}{},le=\"+Inf\"}}", &key[..key.len() - 1], ",")
                };
                out.push_str(&format!("{name}_bucket{merged_inf} {total}\n"));
                out.push_str(&format!("{name}_sum{key} {:.6}\n", state.sums.get(key).copied().unwrap_or(0.0)));
                out.push_str(&format!("{name}_count{key} {total}\n"));
            }
        }
        out
    }
}

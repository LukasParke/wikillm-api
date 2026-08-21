//! Fixed-window per-identity rate limiting, limit read live from runtime
//! settings (0 disables).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Bucket {
    window_start: Instant,
    count: i64,
}

pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { buckets: Mutex::new(HashMap::new()) }
    }

    /// Returns retry-after seconds when limited.
    pub fn check(&self, identity: &str, requests_per_minute: i64) -> Option<u64> {
        if requests_per_minute <= 0 {
            return None;
        }
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let bucket = buckets.entry(identity.to_string()).or_insert(Bucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= Duration::from_secs(60) {
            bucket.window_start = now;
            bucket.count = 1;
            return None;
        }
        bucket.count += 1;
        if bucket.count > requests_per_minute {
            let elapsed = now.duration_since(bucket.window_start);
            Some((Duration::from_secs(60) - elapsed).as_secs() + 1)
        } else {
            // opportunistic cleanup keeps the map bounded
            if buckets.len() > 10_000 {
                buckets.retain(|_, b| now.duration_since(b.window_start) < Duration::from_secs(60));
            }
            None
        }
    }
}

# WikiLLM API Benchmark Results

Run on the local development machine using [autocannon](https://github.com/mcollina/autocannon) via `scripts/benchmark.sh`.

## Test environment

- **OS:** Linux 7.0.11-arch1-1 x86_64
- **CPU:** AMD Ryzen 9 7950X3D 16-Core Processor (32 logical cores)
- **Memory:** 62 GiB
- **Runtime:** Bun 1.3.13
- **Date:** 2026-06-22

## Results summary

| Endpoint                             | Concurrency |     Throughput | Avg latency | p99 latency |
| ------------------------------------ | ----------: | -------------: | ----------: | ----------: |
| `GET /health`                        |          10 | ~104,000 req/s |     0.01 ms |        0 ms |
| `GET /health`                        |          50 | ~102,000 req/s |     0.03 ms |        1 ms |
| `GET /health`                        |         100 |  ~98,000 req/s |     0.42 ms |        1 ms |
| `GET /health`                        |         200 |  ~95,000 req/s |     1.48 ms |        4 ms |
| `GET /v1/pages/wiki/...`             |          10 |  ~44,000 req/s |     0.02 ms |        0 ms |
| `GET /v1/pages/wiki/...`             |          50 |  ~44,000 req/s |     0.68 ms |        2 ms |
| `GET /v1/pages/wiki/...`             |         100 |  ~45,000 req/s |     1.77 ms |       11 ms |
| `PUT /v1/pages/wiki/...` (same page) |           1 |   ~4,600 req/s |     0.01 ms |        0 ms |
| `PUT /v1/pages/wiki/...` (same page) |           5 |   ~5,100 req/s |     0.20 ms |        2 ms |
| `PUT /v1/pages/wiki/...` (same page) |          10 |   ~4,900 req/s |     1.37 ms |        3 ms |
| `POST /v1/ingest`                    |           1 |     ~800 req/s |     0.09 ms |        2 ms |
| `PUT /v1/pages/wiki/{unique}.md`     |           1 |     ~929 req/s |           — |           — |

## Realistic workload benchmarks

`scripts/benchmark-realistic.ts` seeds a 100-page, 20-source wiki and runs
probabilistic clients with think times to mimic real traffic patterns.

| Scenario                                                  | Clients | Think time |   Throughput | p99 latency |
| --------------------------------------------------------- | ------: | ---------: | -----------: | ----------: |
| Read-heavy browsing (health + pages + search)             |     100 |      50 ms | ~3,700 req/s |       16 ms |
| Mixed read/write (70% reads / 30% writes)                 |      50 |     100 ms |   ~970 req/s |        8 ms |
| Write-heavy editing (updates + new notes + sources + log) |      25 |      50 ms |   ~190 req/s |        1 ms |
| Batch ingestion (source + 2 pages + log + index refresh)  |       3 |     200 ms |    ~19 ops/s |       42 ms |
| Observer polling changes/index refresh                    |      10 |     500 ms |     ~7 req/s |    2,750 ms |

## Raw autocannon output

```
GET /health | concurrency=10
  1,144k requests in 11s, 222 MB read
  ~104k req/s, latency p50 0 ms, p99 0 ms

GET /health | concurrency=50
  1,127k requests in 11.01s, 219 MB read
  ~102k req/s, latency p50 0 ms, p99 1 ms

GET /health | concurrency=100
  1,079k requests in 11.01s, 209 MB read
  ~98k req/s, latency p50 0 ms, p99 1 ms

GET /health | concurrency=200
  1,046k requests in 11.02s, 203 MB read
  ~95k req/s, latency p50 1 ms, p99 4 ms

GET /v1/pages/wiki/bench.md | concurrency=10
  480k requests in 11s, 251 MB read
  ~44k req/s, latency p50 0 ms, p99 0 ms

GET /v1/pages/wiki/bench.md | concurrency=50
  481k requests in 11.01s, 251 MB read
  ~44k req/s, latency p50 1 ms, p99 2 ms

GET /v1/pages/wiki/bench.md | concurrency=100
  493k requests in 11.01s, 257 MB read
  ~45k req/s, latency p50 2 ms, p99 11 ms

PUT /v1/pages/wiki/bench.md (same page) | concurrency=1
  50k requests in 11s, 29 MB read
  ~4.6k req/s, latency p50 0 ms, p99 0 ms

PUT /v1/pages/wiki/bench.md (same page) | concurrency=5
  56k requests in 11s, 32.2 MB read
  ~5.1k req/s, latency p50 0 ms, p99 2 ms

PUT /v1/pages/wiki/bench.md (same page) | concurrency=10
  54k requests in 11s, 31.3 MB read
  ~4.9k req/s, latency p50 1 ms, p99 3 ms

POST /v1/ingest | concurrency=1
  8k requests in 10.01s, 2.26 MB read
  ~800 req/s
```

## Observations

- Read-heavy endpoints (`/health`, `GET /v1/pages/...`) scale to tens of thousands of
  requests per second under synthetic load.
- Realistic browsing with think times yields ~3,700 req/s for a mix of health checks,
  page reads, list requests, and search queries.
- Writes to the same file are serialized by the per-path lock; throughput plateaus around
  **5k req/s** for a single hot page under synthetic load, and drops to ~190 req/s in a
  realistic write-heavy mix that also creates new files and appends to the log.
- Creating unique pages is bound by filesystem/metadata overhead; expect **~900 req/s**
  for sustained unique file creation.
- Batch ingest writes a source, multiple pages, appends `log.md`, and refreshes `index.md`;
  realistic throughput is **~19 ingest ops/s** with a small number of concurrent agents.
- Observer-style polling (changes + periodic index refresh) is intentionally low-frequency
  and bounded by index refresh cost; p99 latency is dominated by the refresh operation.
- Real-world throughput will vary based on disk speed, filesystem type, number of files in
  the wiki, and whether external tools (Obsidian, sync clients) are also accessing the folder.

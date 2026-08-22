
# Memory Eval — manual (2026-08-22 20:08:01 UTC)

- Base: `http://127.0.0.1:3930` | run: `1d5bae46`
- Judge: lexical fallback | page mirror: on

| ability | probes | mean score | p50 search ms | p50 query ms | skipped |
|---|---|---|---|---|---|
| fact_recall | 4 | 1.0 | 1.21 | 0.29 | 2 |
| preference_following | 4 | 0.5 | 0.91 | 0.27 | 2 |
| procedural_recall | 2 | 1.0 | 1.12 | 0.3 | 1 |
| latest_state | 2 | 0.5 | 1.49 | 0.39 | 1 |
| abstention | 4 | 0.0 | 1.71 | 0.55 | 2 |
| **overall** | 16 | 0.6 | | | |

## Probe detail

### fact_recall (mean 1.0)
- [None] Which payment provider does the payment-api use for card transactions? — skipped: LLM not configured (search 1.21ms, query 0.29ms)
- [1.0] Which payment provider does the payment-api use for card transactions?; nugget: payment-api processes card transactions through Stripe (search 1.21ms, query 0.29ms)
- [None] How does the auth-service sign users in? — skipped: LLM not configured (search 0.49ms, query 0.23ms)
- [1.0] How does the auth-service sign users in?; nugget: auth-service signs users in by issuing JWT access tokens (search 0.49ms, query 0.23ms)

### preference_following (mean 0.5)
- [None] We are starting a new service tomorrow. Which database engine should we pick? — skipped: LLM not configured (search 0.91ms, query 0.27ms)
- [0.5] We are starting a new service tomorrow. Which database engine should we pick?; nugget: new services should use PostgreSQL, not MySQL (search 0.91ms, query 0.27ms)
- [None] Which database engine does the team prefer for new services? — skipped: LLM not configured (search 0.47ms, query 0.2ms)
- [0.5] Which database engine does the team prefer for new services?; nugget: PostgreSQL instead of MySQL (search 0.47ms, query 0.2ms)

### procedural_recall (mean 1.0)
- [None] How do we rotate the JWT signing keys? — skipped: LLM not configured (search 1.12ms, query 0.3ms)
- [1.0] How do we rotate the JWT signing keys?; nugget: generate a new RS256 keypair, publish it to JWKS, restart auth-service, keep the old key valid for 24 hours (search 1.12ms, query 0.3ms)

### latest_state (mean 0.5)
- [None] Who is the current on-call engineer for search-api? — skipped: LLM not configured (search 1.49ms, query 0.39ms)
- [0.5] Who is the current on-call engineer for search-api?; nugget: Bob Martinez is the current on-call engineer for search-api (search 1.49ms, query 0.39ms)

### abstention (mean 0.0)
- [None] What is the warranty period for the Neptune-9 coffee machine in the office kitchen? — skipped: LLM not configured (search 1.71ms, query 0.55ms)
- [0.0] What is the warranty period for the Neptune-9 coffee machine in the office kitchen? (search 1.71ms, query 0.55ms)
- [None] What venue did the team book for the offsite on the fictional island of Meridia? — skipped: LLM not configured (search 1.01ms, query 0.42ms)
- [0.0] What venue did the team book for the offsite on the fictional island of Meridia? (search 1.01ms, query 0.42ms)


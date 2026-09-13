# Tasks: Network HTTP Capability (Phase 7)

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~500-650 |
| 400-line budget risk | High |
| Chained PRs recommended | Yes |
| Suggested split | PR 1 → PR 2 → PR 3 |
| Delivery strategy | ask-on-risk |
| Chain strategy | stacked-to-main |

Decision needed before apply: Yes
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: High

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Config foundation (REQ-601/2) | PR 1 (base: main) | `cargo test --test config_parsing` | N/A — no runtime boundary | Revert Cargo.toml + capabilities/config |
| 2 | Host fn + bucket + receipts | PR 2 (base: main) | `cargo test --test network_http` | In-process sandbox + TLS stub | Drop `aegis::http_fetch` + 4 fields; fs untouched |
| 3 | gRPC wiring + AD-014 + E2E | PR 3 (base: main) | `cargo test --test grpc_boundary` | In-process tonic + mTLS | Revert handler :121 + guard :310; delete AD-014 |

## Phase 1: Foundation

- [x] 1.1 `Cargo.toml`: add ureq dep; verify rustls 0.23 via `cargo tree -i rustls`
- [x] 1.2 `src/capabilities/mod.rs` (:35-38): `allowed_methods` + `default_allowed_methods()` serde default (REQ-601)
- [x] 1.3 `src/config/mod.rs` (:43-47): mirror `allowed_methods` on `CapabilityDef::NetworkHttp`
- [x] 1.4 `src/config/mod.rs` (:236-252): reject empty hosts/methods, IP-literal/wildcard hosts, zero rate (E-601)
- [x] 1.5 `src/config/mod.rs` (:325-333): normalize hosts→lowercase, methods→uppercase
- [x] 1.6 `tests/fixtures/config/multi_cap.toml`: add `allowed_methods = ["GET"]`

## Phase 2: Core

- [x] 2.1 `src/sandbox/mod.rs` (:171-179): `network_http`/`network_bucket`/`network_agent`/`network_fetch` on `SandboxState`; wire :303 + `from_config`
- [x] 2.2 `src/sandbox/mod.rs`: `TokenBucket` + refill-on-demand `try_take`
- [x] 2.3 `src/sandbox/mod.rs`: `aegis_http_fetch` — endpoint→method→bucket validation; traps S-601/S-602/S-606 (catalog)
- [x] 2.4 `src/sandbox/mod.rs`: fetch stage — 2s connect/5s total → S-603/S-604; `take(1MiB+1)` → S-605; copy ≤ out_len; set `FetchRecord`
- [x] 2.5 `src/sandbox/mod.rs` (:409-411): register `aegis::http_fetch` via `func_wrap`
- [x] 2.6 `src/sandbox/mod.rs`: fetch receipt `network.http`/`fetch`/<URL>/len/`blake3(body)`|`trap`; OOB→`""`, traps→URL; emit-fail→trap (REQ-609)
- [x] 2.7 `src/sandbox/mod.rs`: `#[cfg(feature="test-utils")]` CA-PEM + port override for `https://localhost` tests

## Phase 3: Integration

- [x] 3.1 `src/grpc/handlers/mod.rs` (:121): execute receipt path/result/size from `network_fetch`, fallback `""` (REQ-610)
- [x] 3.2 `src/grpc/handlers/mod.rs` (:310): `msg.starts_with("network ")` first guard; dispatch by second word
- [x] 3.3 `DECISIONS.md`: AD-014 (D1..D6) + Traceability row, citing ADR-013 `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md` (read-only)

## Phase 4: Testing

- [x] 4.1 Create `tests/network_http.rs`: rcgen CA + `tokio-rustls` Accept on ephemeral port; WAT happy GET (E-603)
- [x] 4.2 `tests/network_http.rs`: S-603 refused :443, S-604 stall, S-605 1MiB+1, S-606 burst — trap + receipt asserts; **buffer-too-small trap + trap receipt (AD-005 class, REQ-609)**
- [x] 4.3 Unit tests: E-601 rejects (`src/config/mod.rs`); TokenBucket burst/refill (`src/sandbox/mod.rs`)
- [x] 4.4 `tests/network_http.rs`: E-602 case-insensitive, E-604 exact 1 MiB, E-605 2/1; `ReceiptChain::verify_chain`
- [x] 4.5 `tests/network_http.rs` (harness propio, grpc_boundary intacto): REQ-610 path=URL, result=BLAKE3(body), trap→false + FAILED_PRECONDITION
- [x] 4.6 Gate: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` green

## Dependencies

New runtime dep: `ureq` 3. Test deps rcgen 0.13, tokio-rustls 0.26 already present.
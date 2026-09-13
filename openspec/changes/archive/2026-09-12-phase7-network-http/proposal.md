# Proposal: Network HTTP Capability (Phase 7)

## Intent

Implement the deferred `network.http` capability (ADR-013 item #1): WASM modules become HTTPS clients via a host function with host allowlist, method policy, rate limit, and response size cap. Today `Capability::NetworkHttp(_)` is a NO-OP (`src/sandbox/mod.rs:409-411`) and the gRPC arm returns an empty string — a configurable capability that does nothing. Closes threat-model gaps: network.wasm, TLS-not-mandatory, rate limiting.

## Scope

### In Scope
- `allowed_methods: Vec<String>` on `NetworkHttpParams` (`src/capabilities/mod.rs:35-38`) + `CapabilityDef::NetworkHttp` (`src/config/mod.rs:43-47`), serde default `["GET"]`, retrocompatible
- TLS-only https, fail-closed; rustls 0.23
- Sync host fn via `Linker::func_wrap` (fs_read/fs_write pattern), `ureq` + rustls; connect ~2s, total ~5s
- `allowed_hosts` exact match, case-insensitive; no wildcards/IPs; port 443 implied
- 1 MiB cap → trap S-605; token-bucket rate limit per execution; semaphore max_concurrent=8 unchanged
- Trap receipts S-601..S-606; gRPC wiring + FAILED_PRECONDITION mapping; network config carrier: `SandboxState` holds `Option<NetworkHttpParams>` directly — `CapabilityConfig` untouched (`src/sandbox/mod.rs:87-91` arm stays, unused by network, kept for match exhaustiveness)

### Out of Scope
Fuel metering, two-phase receipts, WASI compat, global rate limiter, TCP/WebSocket, methods beyond `allowed_methods`, plain-HTTP. `DECISIONS.md` untouched — AD entry at design/apply.

## Capabilities

### New Capabilities
- `network-http`: HTTPS client invocations against allowlisted hosts with method allowlist, per-execution rate limit, response size cap; violations trap (S-601 host, S-602 method, S-603 DNS/connect, S-604 timeout, S-605 size, S-606 rate) with trap receipts.

### Modified Capabilities
- None

## Approach

Register `aegis_http_fetch` in the `NetworkHttp` arm (`src/sandbox/mod.rs:409-411`), mirroring fs_read/fs_write (`:382-407`). Guest passes method+URL; host validates scheme=https, host ∈ allowed_hosts, method ∈ allowed_methods, rate bucket, then `ureq` over rustls 0.23. Response > 1 MiB → trap. Receipt: capability_name="network.http", action="fetch", path=<URL>, size=<len>, result=blake3(body) hex | "trap" — no schema change. Async wasmtime rejected (breaks engine async_support for ~77 integration tests).

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/capabilities/mod.rs` | Modified | `allowed_methods` + default fn |
| `src/config/mod.rs` | Modified | CapabilityDef + validation (:236-252) + into_capability (:325-333) |
| `src/sandbox/mod.rs` | Modified | Register host fn :409-411; network config carrier |
| `src/grpc/handlers/mod.rs` | Modified | NetworkHttp arm (:121), trap mapping (:310-319) |
| `src/receipts/mod.rs` | Modified | fetch receipt emission |
| `Cargo.toml` | Modified | add `ureq` (rustls-compatible pin) |
| `tests/` | Modified | network tests; `tests/fixtures/config/multi_cap.toml` |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| ureq/rustls version mismatch | Low | Pin ureq rustls 0.23-compatible; check `cargo tree` |
| SSRF via host matching | Low | Exact hostname only; no wildcards/IPs; https-only |
| Blocking host fn stalls engine | Low | 5s cap; per-exec bucket; existing semaphore |
| Field addition breaks configs | Low | `#[serde(default)]` GET-only |

## Rollback Plan

Revert network arm to NO-OP, gRPC arm to empty string, remove fields + ureq, delete network tests. `cargo test` → 90 green. No receipt schema change to revert.

## Dependencies

- `ureq` pinned rustls 0.23-compatible (rustls 0.23, tokio-rustls already present)
- Reinstates ADR-013 item #1 (`openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md`); AD entry at design/apply.

## Success Criteria

- [ ] `cargo test`: existing 90 + new network tests, zero failures
- [ ] GET to allowed https host returns body; denied host/method trap S-601/S-602
- [ ] http:// traps; > 1 MiB traps S-605; rate limit enforced S-606
- [ ] Fetch receipts carry blake3 body hash, chain-verifiable
- [ ] `cargo clippy -D warnings` and `cargo fmt -- --check` clean
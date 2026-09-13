# Design: Network HTTP Capability (Phase 7)

## Technical Approach

Register a bounded-blocking sync host fn `aegis_http_fetch` in the `NetworkHttp` arm (currently NO-OP, `src/sandbox/mod.rs:409-411`), mirroring `aegis_fs_read` (same `Linker::func_wrap` pattern, sync). Guest passes method+URL as (ptr,len) pairs; host validates endpoint→method→bucket→fetch→size (REQ-603) and traps on every violation with a fetch receipt (REQ-609). HTTP via `ureq` 3 (rustls 0.23 + ring), agent cached per execution in `SandboxState`. Execute-level receipt reads a host-recorded `FetchRecord` so path=BLAKE3-verified URL (REQ-610) — guest is never trusted to report what it fetched. Async wasmtime host fns rejected: turns on engine `async_support`, breaking the ~77 existing sync integration tests (proposal §Approach). Maps 1:1 to S-601..S-606, E-601..E-605.

## Architecture Decisions

| # | Decision | Choice | Alternatives | Rationale |
|---|----------|--------|--------------|-----------|
| D1 | HTTP client | `ureq = { version = "3", default-features = false, features = ["rustls"] }` (verified: ureq ≥2.10.1 uses rustls 0.23, ring provider) | `reqwest::blocking` (hyper stack, pulls tokio runtime); ureq 2.12 | Blocking I/O matches `func_wrap`; pure Rust; ring matches existing deps; `TlsConfig::root_certs(RootCerts::from_pem)` enables hermetic tests; no gzip → body bytes are wire bytes for BLAKE3 |
| D2 | Policy carrier | `SandboxState.network_http: Option<NetworkHttpParams>` (Vía A), plus `network_bucket`, `network_agent`, `network_fetch` fields | Extend `CapabilityConfig` (loses allowed_hosts/methods); closure captures (state needed at runtime) | `CapabilityConfig:from(NetworkHttp)` arm (:87-91) untouched, kept for match exhaustiveness; host fn reads `caller.data()` like :488-494; `Option` → zero cost for non-network sandboxes |
| D3 | Rate limit | `TokenBucket { tokens: f64, last_refill: Instant, rate, capacity }` in `SandboxState`, fresh per execution | Global bucket (out of scope); closure `Cell` (borrow-fragile) | Fresh `Sandbox` per Execute RPC (handlers:84-88) resets bucket → per-execution (REQ-606, E-605); single-threaded store → no Mutex. Refill-on-demand: `min(cap, tokens + elapsed*rate)` |
| D4 | Trap→gRPC | Guard `msg.starts_with("network ")` as the FIRST `if` branch (handlers:310) — structurally isolates all network traps from the fs branches; inside the guard, dispatch by second word: `endpoint`/`method`/`connection`/`timeout`/`size`/`rate` | Error-code envelope (overkill); positional "network-first" insertion into the cascade (order-dependent — rejected) | S-605 message contains "size"/"exceed" which the generic :312 branch would steal; URL is guest-controlled, so S-601 can contain `..`/`size`/`max` and must NEVER be evaluated against fs branches — only a prefix guard makes this structurally safe |
| D5 | REQ-610 URL | Host fn stores `FetchRecord { url, body_blake3, body_len }` on success; handler reads it for the execute receipt (:117-131) | Guest returns URL via WAT result (untrusted — rejected); comma-joined allowlist (rejected after review) | Host is the only party that knows which URL was actually fetched |
| D6 | AD timing | Design records decision; DECISIONS.md `AD-014` transcribed at apply | — | decision-log: docs updated in the same work unit (commit) as the code |

**Trap-path semantics** (REQ-609): guest-memory OOB (URL unreadable) → `path=""` (fs precedent :514); S-601..S-606 → `path=<URL>` — URL is known at every validation point. Execute-level trap receipt stays `""` (:157); REQ-610 constrains only performed fetches.

## Data Flow

```
capabilities TOML → CapabilityDef::NetworkHttp → validate (REQ-602) → into_capability
  (hosts↓lowercase, methods↑uppercase) → Capability::NetworkHttp(params)
  → instantiate_with_capabilities: SandboxState.network_http = Some(params), bucket = new
guest: call aegis_http_fetch(method_ptr,len, url_ptr,len, out_ptr,out_len)
host: read strings ─ OOB → trap("")
  → parse URL (url::Url): scheme=https, no userinfo, host=domain, port∈{None,443} ─ else S-601
  → host ↓lowercase ∈ allowed_hosts ─ else S-601
  → method ↑uppercase ∈ allowed_methods ─ else S-602
  → bucket.try_take() ─ empty → S-606
  → ureq agent (connect 2s / total 5s): DNS/TLS/conn fail → S-603, timeout → S-604
  → body.read take(1MiB+1) ─ >1MiB → S-605
  → blake3(body) → emit fetch receipt (network.http/fetch/<url>/len/hash) ─ emit fail → trap, no data
  → copy ≤out_len to guest memory; set network_fetch; return len
handler: Ok → network_fetch? use (url, body_blake3, body_len) : fallback (:117-122)
        Err → FAILED_PRECONDITION mapping (D4) + execute trap receipt ""
```

## Data Models

```rust
// capabilities/mod.rs — default fn shared with config/mod.rs serde attribute
pub fn default_allowed_methods() -> Vec<String> { vec!["GET".to_string()] }
pub struct NetworkHttpParams {            // + #[serde(default = "default_allowed_methods")]
    pub allowed_hosts: Vec<String>,       // normalized lowercase; no IPs/wildcards
    pub allowed_methods: Vec<String>,     // normalized uppercase
    pub max_requests_per_second: u64,
}
struct FetchRecord { url: String, body_blake3: String, body_len: u64 }
struct TokenBucket { tokens: f64, last_refill: Instant, rate: f64, capacity: f64 }
```

## Interfaces / Contracts

```
(import "aegis" "http_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32) (result i32)))
aegis_http_fetch(method_ptr, method_len, url_ptr, url_len, out_ptr, out_len) -> Result<i32>
```
Returns body length copied to guest memory; traps on all violations (REQ-603).

Trap catalog (exact messages) and gRPC mapping — guard `starts_with("network ")` first (structural boundary), then dispatch by second word:

| Trap | Message | gRPC error_message |
|------|---------|--------------------|
| S-601 | `network endpoint not allowed: <url>` | `network endpoint not allowed` |
| S-602 | `network method not allowed: <method>` | `network method not allowed` |
| S-603 | `network connection failed: <err>` | `network connection failed` |
| S-604 | `network timeout: <stage> > <n>s` | `network timeout exceeded` |
| S-605 | `network response size <n> exceeds limit <max>` | `network response size limit exceeded` |
| S-606 | `network rate limit exceeded: bucket empty` | `network rate limit exceeded` |

## Security

Fail-closed invariants: TLS-only https, exact hostname (no wildcards/IPs), method allowlist, per-execution bucket, 1 MiB body cap (`take` bounds allocation), emit-before-return on success; emit failure → trap + `tracing::error` audit (remote GET already observed by server — same class as AD-005, lesser: no local side effect). Config validation fail-closed (REQ-602). Global semaphore (8) unchanged.

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `Cargo.toml` | Modify | add `ureq = { version = "3", default-features = false, features = ["rustls"] }` |
| `src/capabilities/mod.rs` | Modify | `allowed_methods` field + `default_allowed_methods()` |
| `src/config/mod.rs` | Modify | `CapabilityDef::NetworkHttp` (:43-47) field; validation (:236-252): empty hosts/methods, IP-literal/wildcard hostname, zero rate, case normalization; `into_capability` (:325-333) |
| `src/sandbox/mod.rs` | Modify | `SandboxState` +4 fields; register host fn (:409-411); `aegis_http_fetch`; set-state helper (also `from_config` :449); `#[cfg(feature="test-utils")]` TLS-root/port injection for hermetic tests |
| `src/grpc/handlers/mod.rs` | Modify | NetworkHttp arm (:121); trap map (:310-319, D4); execute receipt from `FetchRecord` (:117-131) |
| `tests/network_http.rs` | Create | WAT modules + TLS stub server tests |
| `tests/fixtures/config/multi_cap.toml` | Modify | add `allowed_methods = ["GET"]` |
| `DECISIONS.md` | Modify | AD-014 at apply (transcribe D1..D6) |

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Unit | Config validation (E-601, IP/wildcard/port in host, zero rate) | `config/mod.rs` tests |
| Unit | TokenBucket refill/burst | sandbox tests |
| Integration | S-601..S-606, E-602..E-605 happy paths | `tests/network_http.rs`: rcgen (dev-dep) CA+cert SAN `localhost`; `tokio-rustls` Accept server on 127.0.0.1:ephemeral; module fetches `https://localhost/x` (URL port-free → 443 validation passes); test-only override redirects socket to test port + injects CA via `RootCerts::from_pem` (URL-policy semantics unchanged). S-603: no override → connect `localhost:443` refused. S-604: server accepts then stalls (total 5s) |
| E2E | REQ-610: execute → path=`https://localhost/ok`, result=BLAKE3(body), size=len; network trap → success=false + mapped message | `tests/grpc_boundary.rs` (mTLS scaffold reused) |
| Receipts | Fetch receipt fields + chain verify (success + each trap) | `tests/network_http.rs` |

## Threat Matrix

N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary. This is a WASM host-function capability inside the existing sandbox; SSRF and DoS vectors are bounded by the allowlist and bucket/cap controls above.

## Migration / Rollout

No migration. `#[serde(default = "default_allowed_methods")]` keeps existing configs (no `allowed_methods`) valid. Network arm was NO-OP; no behavior for existing deployments changes until a config grants `network.http`, and even then `allowed_hosts` gates every fetch. Rollback: revert arm to NO-OP + drop `ureq` (proposal §Rollback).

## Open Questions

- [x] S-604: distinguishing connect vs total timeout — ureq 3 folds both into a transport timeout; design traps either with one message. Confirm acceptable at verify, or add stage detection via `Config::timeout_connect` error context.
  **Resolved at verify/archive (2026-09-12)**: ureq 3 DOES distinguish the stages — `ureq::Timeout::Connect` vs `Timeout::Global` map to distinct trap messages (`network timeout: connect > 2s` / `network timeout: total > 5s`, `src/sandbox/mod.rs:1144-1151`); both disjuncts E2E-tested (`network_stall_timeout_s604` :553, `network_connect_stage_timeout_s604` :581).
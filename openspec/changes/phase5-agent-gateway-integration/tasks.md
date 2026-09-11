# Tasks: Phase 5 Agent Gateway Integration

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | 1200-1800 |
| 400-line budget risk | High |
| Chained PRs recommended | Yes |
| Suggested split | PR 1 → Foundation, PR 2 → gRPC Server, PR 3 → Testing & Polish |
| Delivery strategy | ask-on-risk |
| Chain strategy | stacked-to-main |
| Decision needed before apply | Yes |

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Proto + Config + Emitter + Cargo | PR 1 | `cargo build --bin aegis-runtime` | `cargo test` | Remove proto/, build.rs, config changes |
| 2 | gRPC Server + RPCs + mTLS + Health | PR 2 | `cargo test --test grpc` | `aegis-runtime --config runtime.toml` | Remove src/grpc/, server.rs |
| 3 | Binary + Integration + E2E + Config | PR 3 | `cargo test --all` | `aegis-runtime --config runtime.toml` | Remove bin/aegis-runtime.rs, config example |

---

## Phase 1: Foundation — Proto, Config, Dependencies

- [x] 1.1 Create `proto/aegis/v1/aegis.proto` with `AegisRuntime` service (Execute, VerifyChain, GetReceiptChain), `Health` service, and all message types per design.md protobuf contract
- [x] 1.2 Create `build.rs` with `prost-build` + `tonic-build` to generate protobuf code from `proto/aegis/v1/aegis.proto`
- [x] 1.3 Create `src/config/runtime.rs` with `RuntimeConfig`, `ServerConfig`, `TlsConfig` (cert_path, key_path, ca_cert_path, expected_identity), `ReceiptsConfig`, `ExecutionConfig` (max_concurrent) structs
- [x] 1.4 Modify `src/receipts/mod.rs` to add `public_key()` method on `ReceiptEmitter` returning `ring::signature::PublicKey` — private key never exported
- [x] 1.5 Modify `src/sandbox/mod.rs` — change `SandboxState.receipt_emitter` from `Option<ReceiptEmitter>` to `Option<Arc<Mutex<ReceiptEmitter>>>` for shared single hash chain
- [x] 1.6 Modify `Cargo.toml` — add `tonic`, `prost`, `prost-types`, `rustls`, `tokio-rustls`, `clap`, `tokio-util`, `tower` dependencies; add `[[bin]]` entry for `aegis-runtime` pointing to `src/bin/aegis-runtime.rs`; add `tonic-build` and `prost-build` as build-dependencies

## Phase 2: Core Implementation — gRPC Server, RPCs, mTLS, Key Management

- [x] 2.1 Create `src/grpc/mod.rs` with module declarations for `server`, `handlers`, and `tls`
- [x] 2.2 Create `src/grpc/tls.rs` — rustls server config builder from `TlsConfig`, loads server cert/key, creates `rustls::ServerConfig` with `ClientCertVerifier` using CA cert from `ca_cert_path`, validates CN/SAN against `expected_identity`, returns `INVALID_CERT` gRPC status on mismatch
- [x] 2.3 Create `src/grpc/server.rs` — tonic `AegisRuntime` server implementation: `tonic::transport::Server` with mTLS, `grpc.health.v1.Health` service, `ExecutionConfig.max_concurrent` semaphore limiting Execute RPCs, `RESOURCE_EXHAUSTED` when semaphore full
- [x] 2.4 Create `src/grpc/handlers/mod.rs` — Execute, VerifyChain, GetReceiptChain RPC handlers consolidated (tonic generates single AegisRuntime trait): creates sandbox per request, emits receipt via shared `Arc<Mutex<ReceiptEmitter>>`, returns `ExecuteResponse`/`VerifyChainResponse`/`GetReceiptChainResponse`
- [x] 2.5 Create `src/grpc/handlers/mod.rs` — VerifyChain RPC handler: delegates to `ReceiptChain::verify_chain()`, returns `VerifyChainResponse` with valid+error_message
- [x] 2.6 Create `src/grpc/handlers/mod.rs` — GetReceiptChain RPC handler: returns `GetReceiptChainResponse` with JSON-encoded receipts only (no public key per REQ-721)
- [x] 2.7 Create `src/grpc/handlers/health.rs` — `grpc.health.v1.Health` Check/Watch implementation, returns SERVING when server is healthy
- [x] 2.8 Modify `src/receipts/mod.rs` — `public_key()` method already exists, `Arc<Mutex<ReceiptEmitter>>` thread-safe `emit()` works across concurrent RPCs
- [x] 2.9 Create `src/bin/aegis-runtime.rs` — standalone binary entry point: `--config <path>` CLI via `clap`, loads `RuntimeConfig`, initializes tokio runtime, sets up signal handlers (SIGTERM/SIGINT) for graceful shutdown, drains in-flight RPCs, logs via `tracing`

## Phase 3: Integration — Binary Wiring, Testing, Config, Rollback

- [x] 3.1 Wire `bin/aegis-runtime.rs` — connect CLI config loading → key loading (`load_receipt_keypair`) → TLS setup → gRPC server start → signal handling; fail-closed on any error (non-zero exit)
- [x] 3.2 Create `config/runtime.toml.example` — example config with [server] host/port, [server.tls] cert/key/ca_cert_path/expected_identity, [receipts] key_path, [execution] max_concurrent
- [x] 3.3 Unit tests: `RuntimeConfig` parsing, key loading, 0600 perm check, `ReceiptEmitter` thread-safety under concurrent `emit()`, `ReceiptChain::verify_chain` with valid/tampered/expired receipts
- [x] 3.4 Integration tests: Execute RPC with filesystem.read, mTLS valid client cert accepted, mTLS invalid cert rejected with INVALID_CERT, mTLS missing cert rejected, **concurrency limit: max_concurrent + 1 concurrent Execute requests → request N+1 receives RESOURCE_EXHAUSTED with "max concurrent executions exceeded"**, VerifyChain reuses existing verifier, GetReceiptChain returns chain without public key, graceful shutdown drains in-flight RPCs
- [x] 3.5 E2E test: CLI verifier works with gRPC-emitted receipts — export chain JSON from `GetReceiptChain`, run `aegis-verify`, verify success
- [x] 3.6 Verify `cargo build --bin aegis-runtime` compiles, `cargo test` passes, `cargo clippy -- -D warnings` clean

## Phase 4: Rollback Plan

- [ ] 4.1 Document rollback procedure in tasks.md: remove `bin/aegis-runtime.rs` and `src/grpc/`, revert `Cargo.toml` binary target and tonic/prost deps, delete `proto/aegis/v1/aegis.proto`, delete `config/runtime.toml`, revert `ReceiptEmitter` to library-internal use

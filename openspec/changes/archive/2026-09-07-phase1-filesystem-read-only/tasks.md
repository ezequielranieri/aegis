# Tasks: Phase 1 — Filesystem Read-Only Capability Grant

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~350 |
| 400-line budget risk | Low |
| Chained PRs recommended | No |
| Suggested split | Single PR |
| Delivery strategy | ask-on-risk |
| Chain strategy | pending |

Decision needed before apply: Yes
Chained PRs recommended: No
Chain strategy: pending
400-line budget risk: Low

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|---------------------|-----------------|-------------------|
| 1 | Foundation + CapabilityConfig | PR 1 | `cargo check` | `cargo check` | Revert `SandboxState` + `instantiate` changes |
| 2 | Host function + receipt stub | PR 1 | `cargo clippy -- -D warnings` | `cargo test -- --test-threads=1` | Remove `aegis_fs_read` + `emit_capability_event` |
| 3 | Integration tests (6) | PR 1 | `cargo test -- --test-threads=1` | `cargo test -- --test-threads=1` | Delete new test code from `tests/sandbox.rs` |

## Phase 1: Foundation / Infrastructure

- [x] 1.1 Verify `Cargo.toml` has no `wasi` crate; confirm `wat` is present in `[dev-dependencies]`
- [x] 1.2 Add `CapabilityConfig` struct to `src/sandbox/mod.rs` with `name: String`, `allowed_root: PathBuf`, `max_read_bytes: u64` fields and `From<&Capability>` impl (default root `/data`, default limit 1_048_576)
- [x] 1.3 Extend `SandboxState` with `capabilities: Vec<CapabilityConfig>` field; update `Default` impl

## Phase 2: Core Implementation

- [x] 2.1 Add `emit_capability_event` call stub inside `src/sandbox/mod.rs` host function (calls into receipts module)
- [x] 2.2 Implement `Sandbox::instantiate(wasm_bytes, capabilities)` — convert capabilities to `CapabilityConfig`, store in `SandboxState.capabilities`, build `Linker`, register `aegis::fs_read` per `filesystem.read` capability, compile + instantiate
- [x] 2.3 Implement `aegis_fs_read` host function: validate guest memory bounds (path_ptr/path_len, out_ptr/out_len), read path from guest memory, REQ-103 reject `..` segments before canonicalize, REQ-102 canonicalize + verify within allowed_root, REQ-104 `stat()` size check vs `max_read_bytes`, emit receipt stub BEFORE trap/read, `std::fs::read` → bounds-check out_len → copy to guest memory → return 0; trap with `Trap::new()` on all violations (REQ-107)

## Phase 3: Receipt Stub

- [x] 3.1 Add `emit_capability_event(capability: &str, path: &str, size: u64, result: &str)` to `src/receipts/mod.rs` using `tracing::info!` with structured fields; Phase 3 replaces with signed receipts

## Phase 4: Testing / Verification (6 integration tests + 2 RED unit tests = 8 tests + 1 runner)

- [x] 4.1 RED unit test T-1: traversal rejection logic — path `"../../etc/passwd"` → trap with "path traversal attempt" (pre-canonicalize)
- [x] 4.2 RED unit test T-6: guest memory OOB — invalid `path_ptr`/`path_len` or `out_ptr`/`out_len` → trap
- [x] 4.3 Integration test S-1 (Allowed): `allowed_read.wat` imports `aegis::fs_read`, reads `data/test.txt` (size ≤ max_read_bytes) → returns 0, no trap
- [x] 4.4 Integration test S-2 (Denied): `denied_read.wat` imports `aegis::fs_read`, reads `../secret.txt` → trap (outside allowed_root)
- [x] 4.5 Integration test S-3 (Traversal): `traversal.wat` imports `aegis::fs_read`, reads `../../etc/passwd` → trap (.. segment rejected pre-canonicalize)
- [x] 4.6 Integration test S-4 (FD Leak): `fd_leak.wat` imports `wasi_snapshot_preview1::fd_read` → `Instance::new()` returns Err containing "unknown import" (linking error, not runtime trap)
- [x] 4.7 Integration test T-2 (Symlink Escape): create temp dir with symlink pointing outside `allowed_root`, WAT reads symlink path → trap after canonicalize resolves outside root
- [x] 4.8 Integration test S-5 (Size Exceeded): `size_exceeded.wat` imports `aegis::fs_read`, reads `data/large.bin` (size > max_read_bytes) → trap with "file size exceeds limit"
- [x] 4.9 Run `cargo test -- --test-threads=1` to verify all tests pass (serial required: Wasmtime epoch counter is global to the process; parallel tests with different epoch intervals interfere via shared `Engine` epoch state — AD-004. Temp dir isolation handles filesystem, but epoch state is process-wide and cannot be isolated per test.)

## Phase 5: Cleanup / Documentation

- [x] 5.1 Confirm `wasi` crate absent from `Cargo.toml` and `Cargo.lock`
- [x] 5.2 Add doc comments on fail-closed invariant (REQ-107) to `aegis_fs_read` host function and `Sandbox` struct
- [x] 5.3 Add TOCTOU known limitation comment at `aegis_fs_read`; run `cargo clippy -- -D warnings` and `cargo fmt -- --check`

## Threat Matrix Coverage (1:1 mapping to tasks)

| Row | Threat | Test Task | Phase |
|-----|--------|-----------|-------|
| T-1 | `..` traversal rejection | 4.1 (RED unit) + 4.5 (S-3 integration) | Phase 4 |
| T-2 | Symlink escape | 4.7 (T-2 integration) | Phase 4 |
| T-3 | Size limit bypass | 4.8 (S-5 integration) | Phase 4 |
| T-4 | FD leak / WASI unknown import | 4.6 (S-4 integration) | Phase 4 |
| T-5 | TOCTOU | N/A — documented known limitation | N/A |
| T-6 | Guest memory bounds | 4.2 (RED unit) | Phase 4 |
| T-7, T-8 | VCS/PR automation, Process integration | No | No |

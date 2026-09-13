# ADR-011: test-utils Feature Flag for Signing Failure

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
The `ReceiptEmitter` needs a mechanism to force signing failures in tests to verify fail-closed behavior (S-416, S-702). This requires a test-only method `force_signing_failure()` that is not available in production builds.

### Decision
Use a Cargo feature flag `test-utils` to gate `force_signing_failure()` on `ReceiptEmitter`.

### Rationale
- **Compile-time isolation**: The `#[cfg(feature = "test-utils")]` attribute ensures the `force_signing_failure` field and method are completely absent from release builds. There is zero runtime overhead or attack surface.
- **Dev-dependency pattern**: The feature is enabled only for `aegis = { path = ".", features = ["test-utils"] }` in `[dev-dependencies]`. Production users who depend on `aegis` without this feature get a clean `ReceiptEmitter` without test hooks.
- **Explicit intent**: The feature name `test-utils` signals that anything behind it is test infrastructure, not production API. This prevents accidental use in production code.
- **Alternative considered**: Environment variable `AEGIS_TEST_MODE` (used in `src/grpc/handlers/mod.rs` for epoch interruption) was considered but rejected for `ReceiptEmitter` because env vars are runtime checks, not compile-time guarantees. The feature flag provides stronger isolation.

### Tradeoffs
- Requires `features = ["test-utils"]` on dev-dependency, adding a small cognitive overhead for new contributors
- Two code paths (cfg-gated) increase the surface area of `ReceiptEmitter`
- The `#[cfg(feature = "test-utils")]` pattern must be consistently applied — missed gates could leak test hooks into production

### Consequences
- `cargo test` automatically enables `test-utils` via dev-dependency
- `cargo build --release` does not include test hooks
- `force_signing_failure()` is the only test-only method on `ReceiptEmitter`; all other methods are production-ready

### Traceability
- Source: `Cargo.toml` `[features] test-utils = []`, `src/receipts/mod.rs` `#[cfg(feature = "test-utils")]` blocks
- Tests: `receipt_emitter_signing_failure_forced`, `execute_rpc_signing_failure_via_grpc`
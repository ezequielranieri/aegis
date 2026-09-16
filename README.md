# aegis

**aegis** is a Rust runtime for **secure WebAssembly execution** with capability-based security, signed execution receipts, and a gRPC server with mutual TLS.

> [Español](./README.es.md)
>
> Documentation: [DECISIONS.md](./DECISIONS.md) — architecture decisions & project constitution

> **Status**: Phase 9 (two-phase receipts) implemented — `cargo test` green (160 tests passing), `cargo clippy --all-targets -- -D warnings` clean, `cargo fmt --check` clean.

## What it solves

Running untrusted WebAssembly is only safe when every resource access is gated and every side effect is provable. aegis applies a fail-closed security model to WASM execution:

- **Capability-based host functions** instead of WASI: guests get exactly the capabilities declared in the policy — `filesystem.read`, `filesystem.write`, `network.http` — nothing else. Each capability grants narrow, validated access (e.g., an `allowed_root` for filesystem access or host allowlists for HTTP).
- **Signed, hash-chained execution receipts**: every execution produces an Ed25519-signed receipt (BLAKE3-hashed and chained) that proves *what executed, with what result, at what cost*. Receipts are verifiable offline with `aegis-verify`.
- **Two-phase execution**: `ExecutePrepare` / `ExecuteCommit` / `ExecuteAbort` produce a signed `prepare` receipt before any WASM runs, then a signed `commit` (or `abort`) receipt after — so intent and outcome are both provable.
- **Hard resource limits**: Wasmtime sandbox with 1 MiB memory, 1024 table elements, 4 instances, 2 memories per store, epoch interruption as the wall-clock CPU boundary, and fuel metering for deterministic per-execution CPU accounting.
- **Mutual TLS on the gRPC boundary**: the server requires client certificates signed by a configured CA with CN/SAN matching the expected identity — there is no optional-TLS mode.

## Architecture

```
┌──────────────┐     mTLS gRPC      ┌────────────────────────────────┐
│  agent-gateway │ ───────────────▶ │  aegis-runtime (Rust)            │
│  or any client │                  │  ┌────────────────────────────┐  │
└──────────────┘                    │  │ Execute / Prepare / Commit │  │
                                    │  │ Verify / GetReceiptChain   │  │
                                    │  └────────────┬───────────────┘  │
                                    │               ▼                  │
                                    │  ┌────────────────────────────┐  │
                                    │  │ ReceiptEmitter (Ed25519 +   │  │
                                    │  │ BLAKE3, hash-chained)       │  │
                                    │  └────────────┬───────────────┘  │
                                    │               ▼                  │
                                    │  ┌────────────────────────────┐  │
                                    │  │ Wasmtime sandbox            │  │
                                    │  │  · capability host fns      │  │
                                    │  │  · resource limits          │  │
                                    │  │  · epoch + fuel metering    │  │
                                    │  └────────────────────────────┘  │
                                    └────────────────────────────────┘
```

The runtime is declarative: a TOML configuration file declares server settings, TLS identity, the receipt signing key, policy defaults, and execution knobs (`max_concurrent`, `fuel_budget`). Access control is *deny by default* — a guest only gets the capabilities explicitly granted in its request config.

## Quickstart

Requirements: Rust 1.98 or newer (checked against 1.98.1), edition 2021.

```sh
# Build the binaries
cargo build

# Run the full test suite (no feature flags needed — test-utils is
# auto-enabled via dev-dependencies, see AD-011)
cargo test
```

The two binaries produced:

| Binary | Purpose | Usage |
|--------|---------|-------|
| `aegis-runtime` | gRPC runtime server (mTLS, receipts, sandbox) | `aegis-runtime --config config/runtime.toml.example` |
| `aegis-verify` | Offline receipt chain verifier | `aegis-verify <chain.json> <public_key_b64>` |

## Configuration

Runtime configuration is TOML (`RuntimeConfig`). See [`config/runtime.toml.example`](./config/runtime.toml.example) for the full annotated example:

```toml
[server]
host = "0.0.0.0"
port = 50051

[server.tls]              # mTLS: client cert CN/SAN must match expected_identity
# ca_cert, server_cert, server_key, expected_identity ...

[receipts]
key_path = "/etc/aegis/keys/ed25519-private.pkcs8.pem"

[execution]
max_concurrent = 8        # in-flight executions; exceeded → RESOURCE_EXHAUSTED
fuel_budget = 10_000_000  # per-execution fuel; exhaustion → "fuel budget exceeded"
```

Per-request `PolicyConfig` (TOML) grants capabilities to the guest — e.g. `filesystem.read` with an `allowed_root`, or `network.http` with host/method allowlists.

## Repository layout

```
config/       runtime.toml.example (annotated sample configuration)
openspec/     specs, archived change proposals (openspec-style documentation)
proto/        aegis/v1 protobuf definitions (Execute, Prepare/Commit/Abort, Verify, Health)
src/          runtime: sandbox, receipts, capabilities, policy, grpc, observability
tests/        integration tests: sandbox, config, fuel, network_http, two_phase, gRPC boundary
DECISIONS.md  architecture decision records (AD-001..AD-016)
```

## Documentation

- [DECISIONS.md](./DECISIONS.md) — 16 architecture decision records: fail-closed sandbox behavior (AD-001..AD-004), filesystem hardening (AD-005, AD-006), gRPC boundary (AD-007..AD-009), host functions vs WASI (AD-010), test isolation features (AD-011), mTLS (AD-012), scope cuts (AD-013), network capability (AD-014), fuel metering (AD-015), two-phase receipts (AD-016).
- `openspec/specs/` — requirements and scenarios for each capability.

## License

[MIT](./LICENSE)
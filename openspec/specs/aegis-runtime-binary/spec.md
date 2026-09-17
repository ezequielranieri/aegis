# aegis-runtime Binary Specification

## Purpose

Standalone binary entry point for the aegis runtime gRPC service. Loads configuration, initializes the gRPC server, manages the Ed25519 key lifecycle, and handles graceful shutdown.

## Requirements


| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-800 | System SHALL provide a standalone binary `aegis-runtime` with its own `[[bin]]` entry in `Cargo.toml` | MUST |
| REQ-801 | Binary SHALL accept `--config <path>` CLI argument pointing to TOML runtime config | MUST |
| REQ-802 | Binary SHALL parse `RuntimeConfig` from the TOML file at startup | MUST |
| REQ-803 | Binary SHALL fail-closed and exit with non-zero status if config file is missing or invalid | MUST |
| REQ-804 | Binary SHALL load Ed25519 key pair from `key_path` specified in config | MUST |
| REQ-805 | Binary SHALL start gRPC server on configured `host:port` | MUST |
| REQ-806 | Binary SHALL install SIGTERM and SIGINT signal handlers for graceful shutdown | MUST |
| REQ-807 | Binary SHALL drain in-flight RPCs before exiting on shutdown signal | MUST |
| REQ-808 | Binary SHALL log startup, shutdown, and errors via `tracing` | MUST |
| REQ-809 | `RuntimeConfig` SHALL contain: `[server]` section with `host` (string), `port` (u16), optional `[server.tls]` with `cert_path`, `key_path`, and `ca_cert_path` (CA certificate for mTLS client validation) | MUST |
| REQ-810 | `RuntimeConfig` SHALL contain: `[receipts]` section with `key_path` (string) pointing to Ed25519 key file | MUST |
| REQ-811 | Binary SHALL NOT export, log, or expose the Ed25519 private key in any form | MUST |
| REQ-812 | Binary SHALL be buildable via `cargo build --bin aegis-runtime` | MUST |
| REQ-820 | The `Capability` enum SHALL include a variant for two-phase receipt execution capability. This is **not a new host function capability** — it is a **p | MUST |
| REQ-821 | `PolicyConfig::try_into_capabilities()` SHALL recognize a `two_phase_receipts = true` (or `[capabilities.two_phase_receipts]`) key in the TOML config  | MUST |
| REQ-822 | The `ExecutePrepare` RPC handler SHALL validate that `Capability::TwoPhaseReceipts` is present in the granted capabilities. If not granted, return `FA | MUST |
| REQ-823 | `ExecuteCommit` and `ExecuteAbort` RPCs SHALL NOT re-validate capabilities — they are authorized by virtue of the `prepare_hash` which was issued by a | MUST |

## Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-800 | Startup with valid config | Valid `runtime.toml` with host, port, key_path | `aegis-runtime --config runtime.toml` executed | Server starts on configured host:port, logs startup message |
| S-801 | Startup missing config flag | No `--config` flag provided | `aegis-runtime` executed | Exits with usage error and non-zero status |
| S-802 | Startup invalid config file | `--config` points to malformed TOML | `aegis-runtime --config bad.toml` | Exits with parse error and non-zero status |
| S-803 | Startup missing key file | Config references non-existent key_path | `aegis-runtime --config runtime.toml` | Exits with key load error and non-zero status |
| S-804 | Startup insecure key perms | Key file has 0644 permissions | `aegis-runtime --config runtime.toml` | Exits with permission error and non-zero status |
| S-805 | Graceful shutdown | Server running with active connections | SIGTERM sent | Drains RPCs, logs shutdown, exits 0 |
| S-806 | TLS startup | Config includes valid TLS cert and key paths | `aegis-runtime --config runtime.toml` | Server starts with TLS, accepts TLS connections |
| S-807 | Config with receipts section | Config includes `[receipts] key_path` | `aegis-runtime --config runtime.toml` | ReceiptEmitter initialized, receipts emitted for Execute RPCs |
| S-808 | mTLS client cert validation | Config includes `ca_cert_path`, client connects with valid client cert signed by CA | Client sends ExecuteRequest | Request succeeds, response with receipt |
| S-809 | mTLS rejects invalid client cert | Config includes `ca_cert_path`, client connects with self-signed cert or wrong CA | Client sends ExecuteRequest | Request rejected with INVALID_CERT status, no execution |
| S-810 | mTLS rejects missing client cert | Config includes `ca_cert_path`, client connects without client cert | Client sends ExecuteRequest | Request rejected with INVALID_CERT status, no execution |
| S-970 | TwoPhaseReceipts granted |  Config with `two_phase_receipts = true` |  ExecutePrepare called |  `success=true`, prepare receipt emitted |
| S-971 | TwoPhaseReceipts denied |  Config WITHOUT `two_phase_receipts` |  ExecutePrepare called |  `success=false`, "two-phase receipts capability not granted" |
| S-972 | Legacy Execute works without capability |  Config WITHOUT `two_phase_receipts` |  Legacy Execute called |  Works (backward compat) |
| S-973 | ExecuteCommit authorized by prepare_hash |  Prepare succeeded, commit called |  ExecuteCommit with prepare_hash |  `success=true`, no capability re-check |
| S-974 | ExecuteAbort authorized by prepare_hash |  Prepare succeeded, abort called |  ExecuteAbort with prepare_hash |  `success=true`, no capability re-check |

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-800 |  |
| REQ-801 |  |
| REQ-802 |  |
| REQ-803 |  |
| REQ-804 |  |
| REQ-805 |  |
| REQ-806 |  |
| REQ-807 |  |
| REQ-808 |  |
| REQ-809 |  |
| REQ-810 |  |
| REQ-811 |  |
| REQ-812 |  |
| REQ-820 | S-970, S-971, S-972, S-973, S-974 |
| REQ-821 | S-970, S-971, S-972, S-973, S-974 |
| REQ-822 | S-970, S-971, S-972, S-973, S-974 |
| REQ-823 | S-970, S-971, S-972, S-973, S-974 |

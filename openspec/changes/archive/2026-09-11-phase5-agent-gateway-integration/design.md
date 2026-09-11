# Design: Phase 5 Agent Gateway Integration

## Technical Approach

Implement a gRPC server in a new standalone binary `aegis-runtime` that exposes three RPCs (`Execute`, `VerifyChain`, `GetReceiptChain`) over mTLS. The Ed25519 private key remains in-process within `ReceiptEmitter` (shared via `Arc<Mutex<ReceiptEmitter>>` across concurrent sandboxes). gRPC uses tonic/prost for Rust↔Go interop. Two independent audit chains are maintained: aegis (BLAKE3+Ed25519) and agent-gateway (SHA256).

## Architecture Decisions

### Decision: Separate Binary vs In-Process Library

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Single binary with gRPC feature flag | Simpler deployment, but crash in gRPC brings down main aegis | **Separate binary** — fault isolation aligns with Phase 0 fail-closed principle; aegis crash ≠ gateway crash |
| Shared library + external gRPC wrapper | Clean separation but complex build | Separate binary with own `Cargo.toml [[bin]]` entry |

**Rationale**: The proposal explicitly requires a standalone binary (REQ-800). Fault isolation is critical — if the gRPC server panics, the main aegis library remains usable.

### Decision: Shared ReceiptEmitter Across Sandboxes

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Per-sandbox ReceiptEmitter | Each sandbox has independent chain | **Shared `Arc<Mutex<ReceiptEmitter>>`** — single chain across all concurrent Execute RPCs (REQ-430 modified) |
| Global static ReceiptEmitter | Simpler but harder to test | `Arc<Mutex<ReceiptEmitter>>` owned by server, cloned per-request |

**Rationale**: The spec requires a single hash chain across all executions. `ReceiptEmitter` holds the key pair and chain state; sharing it ensures chain integrity across concurrent RPCs.

### Decision: mTLS Client Identity Validation

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Accept any CA-signed cert | Simpler, less secure | **Validate CN/SAN = `agent-gateway`** (REQ-716) — reject certs from other identities |
| No client cert check | No mTLS benefit | Not acceptable per REQ-713 |

**Rationale**: REQ-716 mandates client certificate identity check. Using CN/SAN `agent-gateway` provides strong binding to the expected caller.

### Decision: Public Key Not in GetReceiptChain Response

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Include public key in chain response | Convenient for callers | **Exclude public key** (REQ-721) — callers obtain it via config/key file |
| Return public key separately | Extra RPC | Not needed — key is static, provisioned out-of-band |

**Rationale**: Keeps gRPC contract minimal and avoids accidental private key exposure. CLI verifier reads public key from key file.

## Data Flow

```
agent-gateway (Go)          aegis-runtime (Rust)
      │                          │
      │  mTLS handshake          │
      │◄────────────────────────►│  (CN=agent-gateway validated)
      │                          │
      │  ExecuteRequest          │
      │  {capability, config}    │
      │─────────────────────────►│
      │                          │  1. Create sandbox per request
      │                          │  2. Load capability config from request
      │                          │  3. Run WASM with host functions
      │                          │  4. Emit signed receipt (ReceiptEmitter)
      │                          │  5. Return ExecuteResponse
      │                          │
      │  ExecuteResponse         │
      │  {result, receipt, ok}   │
      │◄─────────────────────────┤
      │                          │
      │  VerifyChainRequest      │
      │  {receipts, pub_key}     │
      │─────────────────────────►│
      │                          │  ReceiptChain::verify_chain()
      │  VerifyChainResponse     │
      │  {valid, error}          │
      │◄─────────────────────────┤
      │                          │
      │  GetReceiptChainRequest  │
      │  {}                      │
      │─────────────────────────►│
      │                          │  Return ReceiptEmitter.chain()
      │  GetReceiptChainResponse │
      │  {receipts[]}            │  (no public key per REQ-721)
      │◄─────────────────────────┤
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `proto/aegis/v1/aegis.proto` | Create | gRPC service definition with Execute, VerifyChain, GetReceiptChain RPCs |
| `bin/aegis-runtime.rs` | Create | Standalone binary entry point with CLI, config loading, signal handling |
| `src/grpc/server.rs` | Create | gRPC server implementation with tonic, mTLS, request handlers |
| `src/grpc/mod.rs` | Create | Module exports for grpc layer |
| `src/config/runtime.rs` | Create | `RuntimeConfig` with server (host, port, tls) and receipts (key_path) sections |
| `src/receipts/emitter.rs` | Modify | Add `public_key()` method; ensure private key never exported |
| `src/sandbox/mod.rs` | Modify | Accept shared `Arc<Mutex<ReceiptEmitter>>` in `SandboxState` |
| `Cargo.toml` | Modify | Add `tonic`, `prost`, `prost-types`, `rustls`, `tokio-rustls`, `clap` dependencies; add `[[bin]]` for aegis-runtime |
| `build.rs` | Create/Modify | Protobuf code generation via `prost-build` |
| `config/runtime.toml.example` | Create | Example runtime configuration |

## Interfaces / Contracts

### Protobuf Contract (`proto/aegis/v1/aegis.proto`)

```protobuf
syntax = "proto3";

package aegis.v1;

import "grpc/health/v1/health.proto";

service AegisRuntime {
  // Execute a capability and return result + signed receipt
  rpc Execute(ExecuteRequest) returns (ExecuteResponse);

  // Verify a chain of receipts against a public key
  rpc VerifyChain(VerifyChainRequest) returns (VerifyChainResponse);

  // Get the current receipt chain from the runtime
  rpc GetReceiptChain(GetReceiptChainRequest) returns (GetReceiptChainResponse);
}

service Health {
  // Standard gRPC health check
  rpc Check(HealthCheckRequest) returns (HealthCheckResponse);
  rpc Watch(HealthCheckRequest) returns (stream HealthCheckResponse);
}

message ExecuteRequest {
  string capability_name = 1;           // e.g., "filesystem.read"
  bytes config = 2;                     // TOML-encoded PolicyConfig (JSON not supported; caller translates if needed)
}

message ExecuteResponse {
  bool success = 1;
  bytes result = 2;                     // Capability result (raw bytes)
  bytes receipt = 3;                    // JSON-encoded ExecutionReceipt
  string error_message = 4;             // Populated on failure
}

message VerifyChainRequest {
  repeated bytes receipts = 1;          // JSON-encoded ExecutionReceipt[]
  bytes public_key = 2;                 // Ed25519 public key (32 bytes)
}

message VerifyChainResponse {
  bool valid = 1;
  string error_message = 2;             // Populated on failure
}

message GetReceiptChainRequest {
  // Empty per REQ-711
}

message GetReceiptChainResponse {
  repeated bytes receipts = 1;          // JSON-encoded ExecutionReceipt[]
}

// Standard gRPC health check messages (from grpc/health/v1/health.proto)
message HealthCheckRequest {
  string service = 1;
}

message HealthCheckResponse {
  enum ServingStatus {
    UNKNOWN = 0;
    SERVING = 1;
    NOT_SERVING = 2;
    SERVICE_UNKNOWN = 3;
  }
  ServingStatus status = 1;
}
```

### gRPC Error Codes

| Condition | gRPC Status | Details |
|-----------|-------------|---------|
| Invalid/missing client cert | `UNAUTHENTICATED` | `INVALID_CERT` detail (REQ-716) |
| Capability execution violation | `FAILED_PRECONDITION` | `success=false` in response |
| Signing failure | `INTERNAL` | `success=false`, error_message |
| Invalid request format | `INVALID_ARGUMENT` | Protobuf decode error |
| Concurrency limit exceeded | `RESOURCE_EXHAUSTED` | `error_message`: "max concurrent executions exceeded" |
| VerifyChain chain inválida | `OK` | `valid=false` + `error_message` descriptivo en body |

### RuntimeConfig (`src/config/runtime.rs`)

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeConfig {
    pub server: ServerConfig,
    pub receipts: ReceiptsConfig,
    pub execution: ExecutionConfig,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub ca_cert_path: PathBuf,  // Required for mTLS client validation
    pub expected_identity: String,  // Expected CN/SAN of client cert (e.g., "agent-gateway")
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReceiptsConfig {
    pub key_path: PathBuf,  // Ed25519 key file (0600 perms enforced)
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExecutionConfig {
    pub max_concurrent: usize,  // Max concurrent Execute RPCs (semaphore limit)
}
```

### SandboxState Modification

```rust
// In src/sandbox/mod.rs
use std::sync::{Arc, Mutex};
use crate::receipts::ReceiptEmitter;

#[derive(Default)]
pub struct SandboxState {
    limits: StoreLimits,
    capabilities: Vec<CapabilityConfig>,
    // Shared across all sandboxes for single hash chain
    pub receipt_emitter: Option<Arc<Mutex<ReceiptEmitter>>>,
}
```

## Testing Strategy

| Layer | What to Test | Approach |
|-------|--------------|----------|
| Unit | `RuntimeConfig` parsing, key loading, 0600 perm check | `cargo test` with temp files |
| Unit | `ReceiptEmitter` thread-safety under concurrent `emit()` | Spawn multiple tasks calling `emit()`; verify chain integrity |
| Unit | `ReceiptChain::verify_chain` with valid/tampered/expired receipts | Existing tests in `receipts/mod.rs` + new gRPC-specific |
| Integration | Execute RPC with filesystem.read capability | Spin up test server, send gRPC request, verify receipt |
| Integration | mTLS: valid client cert accepted | Generate test CA + client cert, connect with `tonic::transport::Channel` |
| Integration | mTLS: invalid client cert rejected with INVALID_CERT | Self-signed cert, wrong CA, missing cert — verify gRPC status |
| Integration | VerifyChain RPC reuses existing verifier | Feed chain from Execute RPCs to VerifyChain |
| Integration | GetReceiptChain returns chain without public key | Verify response lacks public key field |
| Integration | Graceful shutdown drains in-flight RPCs | Start Execute, send SIGTERM, verify completion before exit |
| E2E | CLI verifier works with gRPC-emitted receipts | Export chain JSON, run `aegis-verify`, verify success |

## Threat Matrix

N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary changes beyond standard gRPC server.

## Migration / Rollout

No migration required. New binary `aegis-runtime` runs independently. Existing aegis library and `aegis-verify` CLI unchanged.

Rollback plan (from proposal):
1. Remove `bin/aegis-runtime.rs` and `src/grpc/` directory
2. Revert `Cargo.toml` binary target and tonic/prost dependencies
3. Delete `proto/aegis/v1/aegis.proto`
4. Delete `config/runtime.toml`
5. `ReceiptEmitter` reverts to library-internal use

## Resolved Design Questions

| # | Question | Resolution | Rationale |
|---|----------|------------|-----------|
| 1 | Exact CN/SAN for `agent-gateway` client cert | **Configurable via `RuntimeConfig`** — `expected_identity` field in `TlsConfig`, not hardcoded. Value set per deploy (dev/staging/prod). | Deployment config, not design decision. Single binary serves all environments. |
| 2 | `ExecuteRequest.config`: TOML vs JSON | **TOML-only** (REQ-707). Consistent with project (all config is TOML). Caller translates JSON→TOML if needed. | Avoids dual parser; single source of truth for config format. |
| 3 | Health check endpoint | **Yes — standard `grpc.health.v1.Health`**. Added to proto. Low risk, standard, avoids custom endpoint later. | Standard practice; enables k8s liveness/readiness probes. |
| 4 | Connection limits / rate limiting | **Configurable semaphore** — `ExecutionConfig.max_concurrent` in `RuntimeConfig`. `Arc<Semaphore>` in Execute RPC; `RESOURCE_EXHAUSTED` if exceeded. | Fail-closed boundary: prevents unbounded concurrent Execute calls from saturating runtime. Tunable via config. |
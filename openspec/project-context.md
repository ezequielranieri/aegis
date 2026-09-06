# Project Context — aegis

**Project**: aegis
**Workspace**: /home/ez/Projects/aegis
**Initialized**: 2026-09-06
**Persistence Mode**: hybrid (OpenSpec + Engram)
**Artifact Store**: both
**Delivery Strategy**: ask-on-risk

## Stack

- **Language**: Rust (edition 2021)
- **Runtime**: Wasmtime 24.0 for WASM sandboxing
- **Async**: Tokio 1.x (full features)
- **Serialization**: Serde + serde_json
- **Observability**: OpenTelemetry 0.27 (OTLP/gRPC exporter, tracing integration)
- **Crypto**: ring (Ed25519), hex encoding
- **Error Handling**: anyhow + thiserror

## Architecture

```
aegis/
├── src/
│   ├── lib.rs              # Main library entry, re-exports
│   ├── main.rs             # Binary entry point
│   ├── sandbox/            # Wasmtime sandboxing
│   ├── capabilities/       # Capability-based security
│   ├── receipts/           # Signed execution receipts + hash-chaining
│   ├── policy/             # Declarative policy engine
│   └── observability/      # OpenTelemetry setup
├── Cargo.toml
└── openspec/               # SDD artifacts (this directory)
```

## Key Components

### Sandbox (`src/sandbox/`)
- Wasmtime Engine + Store management
- Fail-closed defaults (no WASI by default)
- Resource limits: CPU, memory, fuel
- Host function ABI for capability grants

### Capabilities (`src/capabilities/`)
- Capability struct with name + params
- CapabilityProvider trait for host functions
- Built-in capabilities: filesystem.read/write, network.http, crypto.sign/verify

### Receipts (`src/receipts/`)
- ExecutionReceipt: module_hash, input_hash, output_hash, timestamp, previous_receipt_hash, signature
- Ed25519 signing via ring
- Hash-chaining: each receipt commits to previous
- ReceiptChain for audit trail

### Policy (`src/policy/`)
- PolicyRule: capability, effect (Allow/Deny), conditions
- Policy document with version, rules, default_effect
- PolicyEngine: evaluates capabilities against context

### Observability (`src/observability/`)
- Tracing + OpenTelemetry integration
- OTLP exporter to localhost:4317
- Service name: aegis, version from Cargo.toml

## SDD Configuration

- **Mode**: hybrid (OpenSpec files + Engram observations)
- **Strict TDD**: false (no workspace-wide test command yet)
- **Testing**: cargo test, clippy, cargo check, cargo fmt
- **Coverage**: cargo tarpaulin

## Relevant Skills

| Skill | Purpose |
|-------|---------|
| rust-wasmtime-sandbox | Core sandbox implementation patterns |
| crypto-receipts | Signed receipts with hash-chaining |
| declarative-policy-engine | Policy evaluation in host functions |
| observability-engineering | OpenTelemetry, high-cardinality events |
| decision-log | ADRs, known issues, non-implementation |
| work-unit-commits | Reviewable commit/PR sizing |
| branch-pr | Gentle AI PR workflow |

## Next Steps

1. Run `/sdd-explore` to clarify requirements for first change
2. Or run `/sdd-new` to propose initial feature (e.g., sandbox initialization)
3. Implement core sandbox with Wasmtime
4. Add capability host functions
5. Implement receipt signing and chaining
6. Add policy engine
7. Wire up OpenTelemetry observability
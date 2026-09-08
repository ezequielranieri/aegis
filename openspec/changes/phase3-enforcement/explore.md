# Exploration: Phase 3 — Enforcement Beyond Filesystem Read

## Current State

The `Capability` enum has three variants (`FilesystemRead`, `FilesystemWrite`, `NetworkHttp`) with full type definitions, config parsing, and `CapabilityConfig::from` implementations. However, only `filesystem.read` has an enforced host function (`aegis_fs_read`).

In `Sandbox::instantiate_with_capabilities()` (src/sandbox/mod.rs lines 298-319), the match arms for `FilesystemWrite` and `NetworkHttp` are **empty comments** — no host functions are registered. The `CapabilityConfig::from` already handles all three variants (lines 70-91), so the configuration plumbing is complete.

Key files:
- `src/sandbox/mod.rs` — `instantiate_with_capabilities()` has empty match arms for write/network
- `src/capabilities/mod.rs` — `FilesystemWriteParams { allowed_root, max_write_bytes }`, `NetworkHttpParams { allowed_hosts, max_requests_per_second }`
- `src/config/mod.rs` — `CapabilityDef` enum has all three variants with TOML deserialization
- `src/receipts/mod.rs` — `emit_capability_event` stub (tracing::info!) — current receipt mechanism
- `tests/sandbox.rs` — All existing tests use `Capability::FilesystemRead` only

## Affected Areas

- `src/sandbox/mod.rs` — Add host function registrations for `filesystem.write` and `network.http` in `instantiate_with_capabilities()`
- `src/capabilities/mod.rs` — Add `FilesystemWriteParams` and `NetworkHttpParams` method implementations if needed
- `src/receipts/mod.rs` — Integrate signed receipt emission into host function calls
- `tests/sandbox.rs` — Add tests for `filesystem.write` and `network.http` enforcement
- `tests/fixtures/config/` — Add TOML fixtures for write/network capabilities
- `Cargo.toml` — May need additional dependencies (see below)

## Approaches

### Approach 1: Filesystem Write First (Sequential)

Implement `aegis_fs_write` host function first, then `aegis_network_http`.

**Rationale**: Filesystem write has a clearer threat model analogous to the already-implemented `filesystem.read`. The same path validation patterns (canonicalize, check prefix, reject `..`) apply directly. The `FilesystemWriteParams` struct already has `allowed_root` and `max_write_bytes`, mirroring `FilesystemReadParams`.

**Pros:**
- Code reuse: path validation logic from `aegis_fs_read` can be factored and reused
- Same `CapabilityConfig::from` pattern already works
- Threat model is well-understood: write outside allowed_root = security breach
- Minimal new surface area — same WASM import ABI pattern
- Deterministic testability: create files in tempdir, verify writes succeed/fail

**Cons:**
- Sequential delivery means network.http delays
- Write semantics are more complex than read (partial writes, append modes, file creation vs truncation)

**Effort**: Medium (filesystem.write), Medium-High (network.http after)

### Approach 2: Both in Parallel

Implement both host functions simultaneously in the same PR.

**Pros:**
- Completes the Phase 3 scope faster
- Both variants share the same `CapabilityConfig::from` infrastructure
- Test coverage for both can be written together

**Cons:**
- Larger surface area increases risk of bugs
- Network.http introduces async I/O complexity (Tokio) into synchronous host functions
- More moving parts to review
- Harder to isolate issues

**Effort**: High

### Approach 3: Network HTTP First (Alternative)

Start with network.http because it has a clearer boundary model (host allowlist + rate limit).

**Pros:**
- No filesystem symlink/traversal concerns
- Rate limiting is deterministic
- Clear threat model: only allowed hosts, bounded requests

**Cons:**
- Network I/O requires async or blocking calls in host functions — complicates Wasmtime sync ABI
- `Linker::func_wrap` with async host functions requires `wasmtime::component` model or async runtime
- Much more new surface area than filesystem write
- No existing pattern to follow in the codebase

**Effort**: High

## Detailed Analysis

### Threat Model Comparison

| Dimension | filesystem.write | network.http |
|-----------|-----------------|--------------|
| Boundary | `allowed_root` path prefix | `allowed_hosts` allowlist |
| Resource limit | `max_write_bytes` | `max_requests_per_second` |
| Attack surface | Symlink escape, path traversal, file overwrites | DNS rebinding, request smuggling, rate bypass |
| Deterministic testability | High (local filesystem ops) | Lower (network I/O, DNS) |
| TOCTOU risk | Same as filesystem.read | DNS resolution + connection |
| Existing pattern | Directly mirrors `filesystem.read` | No existing pattern |

**Verdict**: `filesystem.write` has the clearer threat model and testability profile. It maps 1:1 to the existing `filesystem.read` pattern.

### Host Function Signature Design

Following the existing `aegis_fs_read` signature pattern:

```rust
// filesystem.write: write bytes from guest memory to a file within allowed_root
fn aegis_fs_write(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,       // guest memory pointer to path string
    path_len: i32,       // length of path string
    data_ptr: i32,       // guest memory pointer to data to write
    data_len: i32,       // number of bytes to write
) -> Result<i32>        // 0 = success, bytes written; trap on error

// network.http: HTTP GET to an allowed host (conceptual — may need async)
fn aegis_network_http(
    mut caller: Caller<'_, SandboxState>,
    url_ptr: i32,        // guest memory pointer to URL string
    url_len: i32,        // length of URL string
    out_ptr: i32,        // guest memory pointer for response buffer
    out_len: i32,        // capacity of output buffer
) -> Result<i32>        // 0 = success, bytes written to out; trap on error
```

Both follow the same ABI: guest memory pointers + lengths, return `Result<i32>`, trap on all violations.

### Integration with `CapabilityConfig::from` and `instantiate_with_capabilities`

The existing `CapabilityConfig::from(&Capability)` already handles all three variants (lines 80-84 for FilesystemWrite, 86-89 for NetworkHttp). The `CapabilityConfig::name` field is correctly set for each variant.

In `instantiate_with_capabilities()`, the match arms need to register host functions:

```rust
Capability::FilesystemWrite(params) => {
    linker.func_wrap("aegis", "fs_write", move |caller, path_ptr, path_len, data_ptr, data_len| {
        aegis_fs_write(caller, path_ptr, path_len, data_ptr, data_len)
    })?;
}
Capability::NetworkHttp(params) => {
    linker.func_wrap("aegis", "network_http", move |caller, url_ptr, url_len, out_ptr, out_len| {
        aegis_network_http(caller, url_ptr, url_len, out_ptr, out_len)
    })?;
}
```

The `capability_configs` vector already stores all `CapabilityConfig` entries in `SandboxState`, so the host function closures can access them via `caller.data().capabilities` — exactly like `aegis_fs_read` does.

### Test Strategy

**filesystem.write tests** (mirroring filesystem.read pattern):
1. **Happy path**: Write within allowed_root, under max_write_bytes → success
2. **Path traversal**: `../evil.txt` → trap
3. **Symlink escape**: symlink pointing outside root → trap (canonicalize catches it)
4. **Size exceeded**: file > max_write_bytes → trap
5. **Guest memory OOB**: data_ptr beyond memory bounds → trap

**network.http tests**:
1. **Happy path**: Allowed host, single request → success
2. **Disallowed host**: Host not in allowed_hosts → trap
3. **Rate limit exceeded**: Too many requests → trap
4. **Invalid URL**: Malformed URL → trap

**Key concern**: `network.http` host function needs HTTP client. Options:
- Use `ureq` or `reqwest` (blocking) — adds dependency
- Use `curl` FFI — adds C dependency
- Use `hyper` (async) — requires async host function or tokio blocking
- For Phase 3, a simple `curl`-based or `ureq`-based blocking call is most pragmatic

### Receipt Emission Integration

The existing `emit_capability_event` stub should be enhanced to emit signed receipts when host functions execute. The question is whether to:
1. Emit a receipt for every host function call (including violations/traps)
2. Only emit receipts for successful operations
3. Emit receipts for both success and violation, but with different metadata

**Recommendation**: Option 3 — emit a receipt for every capability invocation, regardless of outcome. The `ExecutionReceipt` struct already has fields for `module_hash`, `input_hash`, `output_hash`, `timestamp`, `previous_receipt_hash`, `signature`. The receipt should capture the capability name, the action (read/write/http), the path/host, the result (Success/Trap/SizeExceeded), and the size. This ensures the audit trail is complete.

The receipt emission should happen inside each host function, at the same points where `emit_capability_event` is currently called — before the operation for pre-validation failures, after for success.

## Recommendation

**Implement filesystem.write first** using the same sequential pattern as Approach 1. The filesystem.write host function can reuse the path validation logic from `aegis_fs_read` with minimal new code. It has the clearest threat model, the best testability, and the lowest risk. After filesystem.write is proven, network.http can be implemented as a separate follow-up.

The `network.http` implementation should use a blocking HTTP client (e.g., `ureq`) to keep the synchronous host function ABI simple. Async host functions would require the Component model or a complex async runtime integration that is out of scope for Phase 3.

## Risks

- **`network.http` async complexity**: If implemented synchronously with a blocking HTTP client, it ties up a Wasmtime instance thread. If implemented async, it requires the Component model or complex tokio integration.
- **TOCTOU in filesystem.write**: Same as filesystem.read — `stat()` then `write()` has a race window. The existing `aegis_fs_read` documents this as a known limitation. Phase 3 inherits it.
- **Dependency additions**: `network.http` likely requires a new HTTP client dependency (`ureq`, `reqwest`, or `hyper`). This should be evaluated before committing.
- **`ed25519-dalek` not in Cargo.toml**: The task context mentions it but it's absent. The project uses `ring` for Ed25519. The `ExecutionReceipt` struct already uses `ring::signature::Ed25519KeyPair`. This discrepancy should be resolved before Phase 3 receipts work.
- **Receipt hash function**: Current `ExecutionReceipt::hash()` uses `ring::digest::SHA256`. The crypto-receipts skill specifies BLAKE3. No `blake3` dependency exists yet.

## Ready for Proposal

Yes — the analysis is clear enough to proceed to proposal. The main decision is whether to implement both surfaces in parallel or filesystem.write first. Based on the threat model and testability analysis, **filesystem.write first** is the recommended approach. The network.http host function can follow as a separate task.

Key open questions for the proposal:
1. Should `network.http` use a blocking HTTP client or be deferred to a later phase?
2. Should `blake3` replace `SHA256` in the hash chain now?
3. Should `ed25519-dalek` replace `ring` for signatures, or keep `ring`?

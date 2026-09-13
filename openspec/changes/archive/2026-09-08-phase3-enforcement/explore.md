# Exploration: Phase 3 — Enforcement (Surface A)

## Current State

The codebase has a solid foundation for Phase 3 enforcement. Here is the precise current state:

### What is already implemented:
- **`Capability` enum** (`src/capabilities/mod.rs`): All three variants with typed params — `FilesystemRead(FilesystemReadParams)`, `FilesystemWrite(FilesystemWriteParams)`, `NetworkHttp(NetworkHttpParams)`
- **`CapabilityConfig::from(&Capability)`** (`src/sandbox/mod.rs` lines 70-93): Already handles all three variants. Note: `CapabilityConfig` has a single `max_read_bytes: u64` field that is overloaded — for `FilesystemWrite` it receives `max_write_bytes`, for `NetworkHttp` it receives `max_requests_per_second`. This is a design smell to address.
- **`PolicyConfig` validation** (`src/config/mod.rs`): All three capability types have full TOML deserialization, validation (absolute paths, non-zero limits, non-empty host lists), and `try_into_capabilities()` conversion.
- **`ReceiptEmitter`** (`src/receipts/mod.rs`): Fully implemented with `emit(capability, action, path, size, result) -> Result<ExecutionReceipt>`, BLAKE3 hash chain, Ed25519 signing via `ring`. The `force_signing_failure()` test hook works via `#[cfg(feature = "test-utils")]`.
- **`ExecutionReceipt`**: Modern format with `capability_name: String`, `action: String`, `result: String`, `path: String`, `size: u64`, `prev_hash: [u8; 32]`, `signature: [u8; 64]`, `timestamp_ns: u64`. Canonical JSON via `serde_json::to_vec` (struct field order = canonical order).
- **`aegis_fs_read`** (`src/sandbox/mod.rs` lines 468-572): Fully implemented with 5 receipt emission points mirroring S-1..S-5 + S-416. This is the **template** for all enforcement host functions.
- **`SandboxState`**: Has `receipt_emitter: Option<ReceiptEmitter>` and `capabilities: Vec<CapabilityConfig>` — both accessible via `caller.data()` / `caller.data_mut()`.
- **`Sandbox::from_config()`**: Loads keypair from `PolicyConfig.receipts.key_path`, creates `ReceiptEmitter`, stores in `SandboxState`.
- **`Cargo.toml`**: Already has `blake3 = "1.0"`, `ring = "0.17"`, `tokio` (full features), `anyhow`, `serde_json`, `base64`, `tempfile` — all dependencies needed are present.
- **`tests/sandbox.rs`**: Has comprehensive S-1..S-5 + S-416 receipt tests for `filesystem.read` only.

### What is NOT implemented (the enforcement gap):
- **`aegis_fs_write`**: Empty match arm in `instantiate_with_capabilities()` (line 393-395)
- **`aegis_network_http`**: Empty match arm in `instantiate_with_capabilities()` (line 396-398)
- **No `filesystem.write` tests** in `tests/sandbox.rs` — only `capability_config_from_filesystem_write` unit test exists
- **No `network.http` tests** at all
- **`CapabilityConfig` field naming**: `max_read_bytes` is overloaded for write and network limits — this needs a rename or structural fix.

### Key architectural pattern (from `aegis_fs_read`):

```
1. Get capability config from caller.data().capabilities by name
2. Validate guest memory bounds
3. Read path/data from guest memory
4. Validate against allowed_root/allowed_hosts
5. Emit receipt via caller.data_mut().receipt_emitter.as_mut()?.emit(...)
6. If emit() returns Err → immediately return Trap (fail-closed, Decision 4)
7. Execute operation
8. Return success or trap
```

## Affected Areas

- `src/sandbox/mod.rs` — Add `aegis_fs_write` and `aegis_network_http` functions, register in `instantiate_with_capabilities()`, fix `CapabilityConfig::from` field naming
- `src/capabilities/mod.rs` — Potentially rename `max_read_bytes` to `max_bytes` in `CapabilityConfig` or add separate fields
- `src/receipts/mod.rs` — No changes needed (already supports all receipt patterns)
- `src/config/mod.rs` — No changes needed (already validates all three types)
- `tests/sandbox.rs` — Add S-1..S-5 + S-416 tests for `filesystem.write` and `network.http`
- `tests/fixtures/config/` — Add TOML fixtures for write/network capabilities

## Approaches

### Approach 1: Sequential — filesystem.write first, then network.http

Implement `aegis_fs_write` first using the exact same pattern as `aegis_fs_read`, then implement `aegis_network_http`.

**Pros:**
- `filesystem.write` maps 1:1 to `filesystem.read` — same path validation, same `CapabilityConfig`, same receipt emission pattern
- Code reuse: the `..` rejection, canonicalize, prefix check, and size validation patterns are identical
- `CapabilityConfig::from` already handles both variants
- Lower risk — proven pattern from `aegis_fs_read`
- `network.http` can be designed after `filesystem.write` is proven

**Cons:**
- Sequential delivery delays `network.http`
- `CapabilityConfig.max_read_bytes` field name is misleading for write/network — needs fix either way

**Effort**: Medium (filesystem.write), High (network.http)

### Approach 2: Parallel — both in same implementation pass

Implement both host functions simultaneously.

**Pros:**
- Completes Phase 3 scope faster
- Both share `CapabilityConfig::from` infrastructure
- Test coverage for both written together

**Cons:**
- Larger surface area increases bug risk
- `network.http` introduces async/blocking I/O complexity into sync host functions
- Harder to isolate issues between the two
- `network.http` has no existing codebase pattern to follow

**Effort**: High

### Approach 3: Fix `CapabilityConfig` first, then implement

Address the `max_read_bytes` field naming issue before implementing host functions.

**Pros:**
- Clean API — `CapabilityConfig` would have `max_bytes: u64` or separate `max_read_bytes`/`max_write_bytes`/`max_requests_per_second`
- Prevents future confusion

**Cons:**
- Adds refactoring overhead before implementation
- `CapabilityConfig::from` and all call sites need updating
- Risk of breaking existing tests

**Effort**: Low+Medium

## Detailed Analysis

### Threat Model: filesystem.write

| Attack | Mechanism | Defense |
|--------|-----------|---------|
| Path traversal via `..` | Guest writes `../../etc/passwd` | Reject `..` segments pre-canonicalize (same as read) |
| Symlink escape | Symlink inside root points outside | `canonicalize()` + `starts_with()` check catches it |
| Write outside allowed_root | Canonical path not under root | `starts_with(canonical_root)` check |
| Size exceed | File > `max_write_bytes` | `stat()` then compare to `max_bytes` |
| Partial write / corruption | Incomplete write leaves file in bad state | Atomic write: write to temp file, then `rename()` |
| Guest memory OOB | `data_ptr` beyond memory bounds | Wasmtime memory bounds check |
| Overwrite existing file | Write destroys existing data | Decision needed: allow overwrite or reject? |
| TOCTOU | `stat()` then write — race window | Inherited from `filesystem.read` limitation |

**Key difference from read**: Write needs **atomic write** — write to temp file in allowed_root, then `std::fs::rename()` to target. This prevents partial writes from corrupting existing files. The temp file must also be within allowed_root.

**Open question**: Should `filesystem.write` create new files or only overwrite existing ones? The `std::fs::write` behavior creates files if they don't exist. For a sandbox, this is probably acceptable as long as the path is within allowed_root.

### Threat Model: network.http

| Attack | Mechanism | Defense |
|--------|-----------|---------|
| DNS rebinding | Allowed host resolves to different IP | Resolve host at config time, pin IPs |
| Request smuggling | Malformed HTTP request | Use HTTP client library that parses strictly |
| Rate limit bypass | Rapid requests exceed `max_requests_per_second` | Track request count per second, trap on exceed |
| Disallowed host | Connect to host not in `allowed_hosts` | Check URL host against allowlist before connecting |
| Timeout abuse | Connection hangs forever | Enforce timeout per request |
| Response size exceed | Response body > limit | Enforce `max_response_bytes` |
| SSL/TLS bypass | Redirect to non-HTTPS | Disable redirects, enforce HTTPS |
| IPv6/IPv4 bypass | Host allowlist only covers one protocol | Normalize to canonical host, check both |

**Key difference from filesystem**: Network operations require an HTTP client. `tokio` is already a dependency with `full` features, but `aegis_fs_read` uses synchronous WASM host functions. Options for HTTP:

- **`ureq`** (blocking): Simple, adds one dependency, works in sync host function
- **`reqwest`** (blocking): More features, but heavier dependency tree
- **`hyper`** (async): Would require async host functions or tokio blocking — complex
- **`curl` FFI**: Adds C dependency — avoid

**Recommendation**: Use `ureq` for blocking HTTP in the synchronous host function. It's simple, well-maintained, and keeps the WASMtime ABI clean. Alternatively, since `tokio` is already a dependency with `full` features, use `tokio::runtime::Handle::current().block_on()` to run async HTTP calls inside the sync host function — but this adds complexity and potential deadlock risk.

**Open question**: Does `network.http` need to support POST/PUT, or only GET? The current `NetworkHttpParams` has no method restriction. Should the config specify allowed methods?

### Config Schema Design

The TOML config already supports all three types in `CapabilityDef`:

```toml
[[capabilities]]
name = "filesystem.write"
allowed_root = "/data"
max_write_bytes = 1048576

[[capabilities]]
name = "network.http"
allowed_hosts = ["example.com", "api.example.com"]
max_requests_per_second = 100
```

**Issue**: `CapabilityConfig` in `src/sandbox/mod.rs` uses `max_read_bytes: u64` for all three variants. For `FilesystemWrite`, it gets `max_write_bytes`; for `NetworkHttp`, it gets `max_requests_per_second`. This conflation should be addressed:

**Option A**: Rename `max_read_bytes` to `max_bytes` in `CapabilityConfig` — simple rename, all call sites update.

**Option B**: Add separate fields: `max_read_bytes: u64`, `max_write_bytes: u64`, `max_requests_per_second: u64`. More verbose but semantically clear.

**Option C**: Keep the overloaded field but rename it to `max_limit_bytes` or `rate_limit` — still not perfect.

**Recommendation**: Option A (`max_bytes`) is the minimal change. The field represents "the maximum resource limit for this capability" regardless of whether it's bytes or requests/second. However, this does mean the unit semantics differ — `max_bytes` for filesystem means bytes, for network means requests/second. This is documented in the `CapabilityConfig` struct doc comment.

### Host Function Signatures

Following the exact ABI pattern from `aegis_fs_read`:

```rust
// filesystem.write: write bytes from guest memory to file within allowed_root
// Uses atomic write (temp file + rename) within allowed_root
fn aegis_fs_write(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,       // guest memory pointer to path string
    path_len: i32,       // length of path string
    data_ptr: i32,       // guest memory pointer to data to write
    data_len: i32,       // number of bytes to write
) -> Result<i32> {
    // Return 0 on success, trap on any violation
}

// network.http: HTTP GET to an allowed host
// Uses blocking HTTP client (ureq) with timeout and size limits
fn aegis_network_http(
    mut caller: Caller<'_, SandboxState>,
    url_ptr: i32,        // guest memory pointer to URL string
    url_len: i32,        // length of URL string
    out_ptr: i32,        // guest memory pointer for response buffer
    out_len: i32,        // capacity of output buffer
) -> Result<i32> {
    // Return bytes written on success, trap on violation
}
```

Both follow the same ABI: guest memory pointers + lengths, return `Result<i32>`, trap on all violations. The `network.http` function needs to handle URL parsing, host allowlist checking, and response buffering.

### Integration Points with ReceiptEmitter

The receipt emission pattern from `aegis_fs_read` is the template. Each validation point in the new host functions must emit a receipt:

**`aegis_fs_write` receipt points** (mirroring `aegis_fs_read`):

| Point | Scenario | `result` param | `size` param |
|-------|----------|----------------|--------------|
| After capability config load | Before any validation | — | — |
| Guest memory OOB | `data_ptr` out of bounds | `"trap"` | `0` |
| `..` segment detected | Path traversal attempt | `"trap"` | `0` |
| Canonicalize fails | Path outside allowed_root | `"trap"` | `0` |
| Size exceeds `max_write_bytes` | `stat()` > limit | `"trap"` | `file_size` |
| Happy path | Before write | `"success"` | `file_size` |

**`aegis_network_http` receipt points**:

| Point | Scenario | `result` param | `size` param |
|-------|----------|----------------|--------------|
| After capability config load | Before any validation | — | — |
| Guest memory OOB | `url_ptr` out of bounds | `"trap"` | `0` |
| Invalid URL | URL parsing fails | `"trap"` | `0` |
| Host not in allowlist | URL host not in `allowed_hosts` | `"trap"` | `0` |
| Rate limit exceeded | Too many requests | `"trap"` | `0` |
| Happy path | After successful response | `"success"` | `response_bytes` |

**Access pattern** (from `aegis_fs_read`, lines 476-482):
```rust
let cap = caller
    .data()
    .capabilities
    .iter()
    .find(|c| c.name == "filesystem.write")
    .ok_or_else(|| anyhow::anyhow!("filesystem.write capability not granted"))?
    .clone();
let cap_name = cap.name.clone();
```

Then for receipt emission:
```rust
if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
    if let Err(e) = emitter.emit(&cap_name, "write", path, file_size, "trap") {
        return Err(anyhow::anyhow!("receipt emission failed: {}", e));
    }
}
```

**Critical fail-closed rule** (Decision 4): If `emit()` returns `Err`, the host function MUST immediately return `Trap`. The original operation result (success data or violation trap) is discarded. No data is returned to the caller.

### `CapabilityConfig` Field Naming Issue

Current `CapabilityConfig` struct:
```rust
pub struct CapabilityConfig {
    pub name: String,
    pub allowed_root: PathBuf,
    pub max_read_bytes: u64,  // Overloaded!
}
```

And its `From<&Capability>` impl:
```rust
Capability::FilesystemWrite(params) => Self {
    name: "filesystem.write".to_string(),
    allowed_root: ...,
    max_read_bytes: params.max_write_bytes,  // Misleading field name
},
Capability::NetworkHttp(params) => Self {
    name: "network.http".to_string(),
    allowed_root: PathBuf::new(),
    max_read_bytes: params.max_requests_per_second,  // Very misleading
},
```

**Recommended fix**: Rename `max_read_bytes` to `max_bytes` in `CapabilityConfig`, and add a doc comment explaining the field represents the capability's maximum resource limit (bytes for filesystem, requests/second for network). This is a one-line rename plus doc comment update. All `CapabilityConfig::from` implementations need to use `max_bytes` instead of `max_read_bytes`.

However, `aegis_fs_read` currently reads `cap.max_read_bytes` — this also needs updating.

## Test Strategy

### filesystem.write Tests (mirroring S-1..S-5 + S-416 pattern)

All tests use WAT modules that import `aegis.fs_write` and call it with guest memory pointers.

| Test | Scenario | WAT module | Expected |
|------|----------|------------|----------|
| S-1-W | Happy path: write within root, under limit | Write "hello" to file in allowed_root | Success, receipt result="success" |
| S-2-W | Path outside root: write to `../evil.txt` | Path escapes allowed_root | Trap, receipt result="trap" |
| S-3-W | Path traversal: `../../etc/passwd` | `..` segments pre-canonicalize | Trap, receipt result="trap" |
| S-5-W | Size exceeded: write > max_write_bytes | Data exceeds limit | Trap, receipt result="trap" |
| S-416-W | Signing failure: force_signing_failure | Happy path but emit fails | Trap, no data returned, no receipt added |
| T-6-W | Guest memory OOB: data_ptr beyond bounds | data_ptr out of range | Trap, receipt result="trap" |
| Symlink-W | Symlink escape inside root | Symlink points outside root | Trap (caught by canonicalize) |

**Key WAT patterns**: Need `data_ptr` and `data_len` parameters in the import signature. The WAT module must allocate guest memory for both the path string and the data to write.

**Atomic write implementation**: `aegis_fs_write` should:
1. Validate path, reject `..`, canonicalize
2. Check size against `max_write_bytes`
3. Emit success receipt
4. Write data to temp file in allowed_root (e.g., `target.write_tmp`)
5. `std::fs::rename(temp, target)` for atomicity
6. If any step fails, emit trap receipt and bail

### network.http Tests

| Test | Scenario | Expected |
|------|----------|----------|
| S-1-N | Happy path: allowed host, single request | Success, receipt result="success" |
| S-2-N | Disallowed host: host not in `allowed_hosts` | Trap, receipt result="trap" |
| S-3-N | Rate limit exceeded: too many requests | Trap, receipt result="trap" |
| S-5-N | Response too large: exceeds `max_bytes` | Trap, receipt result="trap" |
| S-416-N | Signing failure: force_signing_failure | Trap, no data returned |
| T-6-N | Guest memory OOB: url_ptr beyond bounds | Trap, receipt result="trap" |
| Invalid URL | Malformed URL | Trap, receipt result="trap" |

**Key challenge**: `network.http` tests need an HTTP server. Options:
- Use `tempfile` + `std::net::TcpListener` to create a local HTTP server — no external dependency
- Use `ureq` to make requests to `localhost` — the local server can be started in the test
- Mock the HTTP client — complex, adds indirection

**Recommendation**: Use `std::net::TcpListener` to spawn a local HTTP server in each test. This avoids external HTTP dependencies in tests and is fully deterministic.

### Regression Tests

- All existing `filesystem.read` tests (S-1..S-5, S-416) must continue to pass
- All existing receipt chain tests (S-400..S-414, S-416) must pass
- `capability_config_from_filesystem_write` and `capability_config_from_network_http` tests must pass with updated field names
- `capability_enum_serialization_roundtrip` must pass

### Test Fixture Files

Add TOML config fixtures for write/network testing:

```toml
# tests/fixtures/config/filesystem-write.toml
[[capabilities]]
name = "filesystem.write"
allowed_root = "/tmp/aegis-test-write"
max_write_bytes = 1048576
```

```toml
# tests/fixtures/config/network-http.toml
[[capabilities]]
name = "network.http"
allowed_hosts = ["localhost"]
max_requests_per_second = 10
```

## Open Questions and Decisions Needed

### 1. `CapabilityConfig` field naming
**Question**: Should `max_read_bytes` be renamed to `max_bytes`, or should `CapabilityConfig` get separate fields?
**Impact**: Affects `src/sandbox/mod.rs`, `src/config/mod.rs`, all test code.
**Recommendation**: Rename to `max_bytes` — minimal change, clear enough with doc comment.

### 2. `filesystem.write` overwrite behavior
**Question**: Should writing to an existing file be allowed, or should it be rejected?
**Impact**: Affects `aegis_fs_write` logic. If rejection is desired, add an `overwrite` flag to `FilesystemWriteParams`.
**Recommendation**: Allow overwrite (like `std::fs::write`). If later phases need to prevent overwrites, add a config flag.

### 3. `network.http` method restrictions
**Question**: Should the config specify allowed HTTP methods (GET only? POST too?)?
**Impact**: Affects `NetworkHttpParams` struct and `CapabilityDef::NetworkHttp` variant.
**Recommendation**: Add `allowed_methods: Vec<String>` to `NetworkHttpParams` and `CapabilityDef::NetworkHttp`. Default to GET only if not specified. This aligns with the task requirement of "method restrictions."

### 4. `network.http` response size limit
**Question**: Should there be a separate `max_response_bytes` field, or reuse `max_requests_per_second`?
**Impact**: Affects `NetworkHttpParams` struct and `CapabilityConfig::from`.
**Recommendation**: Add `max_response_bytes: u64` to `NetworkHttpParams`. The current `max_requests_per_second` is a rate limit, not a size limit. Having both is clearer.

### 5. HTTP client dependency
**Question**: Which blocking HTTP client to use? `ureq`, `reqwest`, or something else?
**Impact**: Adds a new dependency to `Cargo.toml`.
**Recommendation**: `ureq` — simple, lightweight, no async runtime needed. If `tokio` is sufficient for the project's needs, `reqwest` with blocking mode is also acceptable since `tokio` is already a dependency.

### 6. `network.http` timeout configuration
**Question**: Should there be a configurable timeout per request, or a fixed default?
**Impact**: Affects `NetworkHttpParams` struct.
**Recommendation**: Add `timeout_seconds: u64` to `NetworkHttpParams` with a reasonable default (e.g., 30s). This prevents hanging connections from exhausting resources.

### 7. Atomic write temp file naming
**Question**: How to name the temp file for atomic writes?
**Impact**: Affects `aegis_fs_write` implementation.
**Recommendation**: Use the target filename with `.aegis_tmp` suffix (e.g., `file.txt.aegis_tmp`). Write to temp, then rename to target. The temp file is cleaned up on failure.

### 8. Receipt emission for `network.http`
**Question**: What is the `action` string for HTTP receipts — `"http"`, `"GET"`, or `"network.http"`?
**Impact**: Affects `ExecutionReceipt.action` field value and test assertions.
**Recommendation**: Use `"http"` as the action string (consistent with the pattern where `capability_name` is `"network.http"` and `action` describes the operation). For method-specific receipts, use the HTTP method string (e.g., `"GET"`, `"POST"`).

### 9. `CapabilityConfig::from` for `NetworkHttp` — what does `max_read_bytes` represent?
**Current code**: `CapabilityConfig::from(&Capability::NetworkHttp)` sets `max_read_bytes = params.max_requests_per_second`.
**Issue**: After renaming `max_read_bytes` to `max_bytes`, this becomes `max_bytes = max_requests_per_second` which is semantically confusing.
**Recommendation**: Add a separate `max_requests_per_second` field to `CapabilityConfig` alongside `max_bytes`. This is Option B from the field naming discussion above.

### 10. Thread safety of `ReceiptEmitter` mutation
**Question**: Is `caller.data_mut()` sufficient for `ReceiptEmitter` mutation in host functions?
**Answer**: Yes — Wasmtime host functions are single-threaded per instance. `caller.data_mut()` gives exclusive mutable access to `SandboxState`. The `ReceiptEmitter` mutation is safe. This was confirmed in the Phase 3 receipts design (Decision).

## Recommendation

**Use Approach 1** (sequential — filesystem.write first, then network.http), with the following priorities:

1. **Fix `CapabilityConfig` field naming first**: Rename `max_read_bytes` to `max_bytes` and add `max_requests_per_second` as a separate field in `CapabilityConfig`. Update `CapabilityConfig::from` for all three variants. This is a prerequisite for clean implementation.

2. **Implement `aegis_fs_write`**: Mirror `aegis_fs_read` exactly with the addition of atomic write (temp file + rename). All 5+ receipt emission points from `aegis_fs_read` apply.

3. **Add `filesystem.write` tests**: Mirror the S-1..S-5 + S-416 WAT test pattern. Use `tempfile` for filesystem fixtures.

4. **Implement `aegis_network_http`**: After `filesystem.write` is proven, add the network host function with `ureq` (or `reqwest` blocking). Use `std::net::TcpListener` for test servers.

5. **Add `network.http` tests**: Same receipt emission pattern, adapted for network-specific validation points.

6. **Add config fixtures**: TOML files for write/network capability testing.

## Risks

- **`CapabilityConfig` refactoring**: Renaming `max_read_bytes` to `max_bytes` touches many call sites. Low risk if done carefully, but must update all tests.
- **`network.http` dependency addition**: `ureq` or `reqwest` adds surface area. Both are well-audited but still new dependencies.
- **Atomic write edge cases**: If `rename()` fails (cross-device, permissions), the temp file is left behind. Need cleanup logic.
- **HTTP test server complexity**: Using `std::net::TcpListener` for test servers adds test complexity. Must handle HTTP parsing correctly.
- **TOCTOU in filesystem.write**: Same inherited limitation as `filesystem.read`. `stat()` then `write()` has a race window.
- **Blocking HTTP in sync host function**: Ties up a Wasmtime instance thread during HTTP request. For Phase 3 this is acceptable; async would require Component model.
- **`max_bytes` semantic confusion**: Even after renaming, `max_bytes` means different things for filesystem (bytes) vs network (requests/second). Document clearly in struct doc comments.
- **Receipt chain integrity under concurrent access**: Not a concern — Wasmtime host functions are single-threaded per instance.

## Ready for Proposal

Yes — the analysis is clear. The codebase is more complete than the existing explore.md suggests: `ReceiptEmitter`, `ExecutionReceipt`, `blake3`, `ring`, `tokio`, and the full `aegis_fs_read` pattern are all already implemented. The enforcement gap is specifically the two empty match arms in `instantiate_with_capabilities()` and the missing host function implementations.

Key decisions needed before proposal:
1. Fix `CapabilityConfig` field naming (`max_read_bytes` → `max_bytes` + separate `max_requests_per_second`)
2. Choose HTTP client dependency (`ureq` vs `reqwest` blocking)
3. Add `allowed_methods` and `timeout_seconds` to `NetworkHttpParams`
4. Decide `filesystem.write` overwrite behavior
5. Decide atomic write temp file naming strategy

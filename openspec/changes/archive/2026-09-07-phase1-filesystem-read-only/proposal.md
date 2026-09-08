# Proposal: Filesystem Read-Only Capability Grant (Phase 1)

## Intent

Introduce the first capability grant — `filesystem.read` — enabling WASM modules to read files from a single fixed root directory. This is the foundational capability for all future filesystem, network, and crypto grants. The runtime (not WASI) decides access via custom host functions, ensuring receipts can prove exactly what capability was used and with what parameters.

## Scope

### In Scope
- Custom host function `aegis::fs_read` via `Linker::func_wrap` (no WASI)
- `Capability` struct usage: `name: "filesystem.read"`, `params: { "allowed_root": "/data", "max_read_bytes": 1048576 }`
- Path validation against `allowed_root` in host; trap on traversal or escape
- Size limit enforcement: `stat()` before read, trap if file size > `max_read_bytes`
- `Sandbox::instantiate()` accepts `&[Capability]` and registers host functions
- 5 hostile WAT modules covering: allowed read, denied read, path traversal, fd leak, size limit exceeded
- Receipt emission stub: log capability name + path + size (Phase 3 extends)
- Remove `wasi` crate from Cargo.toml (not needed, not transitive)

### Out of Scope
- Declarative policy engine (Phase 2)
- Multi-root or writable filesystem capabilities
- Receipt signing/hash-chaining (Phase 3)
- Network, crypto, or other capabilities
- WASI Preview 1/2 compatibility layer

## Capabilities

### New Capabilities
- `filesystem-read`: Read-only file access within a single configured root directory, with configurable size limit. Host function `aegis::fs_read(path: &str) -> Result<Vec<u8>, Trap>` validates path against allowed_root, checks file size against max_read_bytes via `stat()`, reads via `std::fs::read`, traps on any violation (path outside root, traversal attempt, size exceeded, or IO error).

### Modified Capabilities
- None

## Approach

**Custom host functions via `func_wrap` ONLY — NO WASI.** This aligns with the core architectural decision: the runtime decides what's allowed, not WASI. A single host function `aegis::fs_read` is registered via `Linker::func_wrap` during `Sandbox::instantiate()` when the `filesystem.read` capability is present.

**Host function signature** (wasmtime 24.0):
```rust
fn aegis_fs_read(
    caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32  // 0 = success, traps on error
```

**Path and size validation logic**:
1. Read path string from guest memory at `path_ptr`/`path_len`
2. Resolve to absolute path, canonicalize
3. Verify path starts with `allowed_root` (from capability params)
4. Reject any `..` segments in the guest-provided path
5. `stat()` the file → if `size > max_read_bytes` (from capability params): `Trap::new("file size exceeds capability limit")`
6. If valid: `std::fs::read(path)` → copy bytes to guest `out_ptr`/`out_len` (verify `out_len` >= file size)
7. If invalid: `Trap::new("filesystem access denied: path outside allowed root or size limit exceeded")`

**Known Limitation (TOCTOU)**: There is a race window between `canonicalize()` + `stat()` and `std::fs::read()` where the file could be replaced (e.g., symlink swap). Phase 1 scope is single-threaded, single-root; this is documented as a known limitation. Phase 2/3 will address with `openat` + file descriptor passing or policy-enforced immutable roots.

**Sandbox integration**:
- `Sandbox::instantiate(wasm_bytes: &[u8], capabilities: &[Capability]) -> Result<Instance>`
- Iterate `capabilities`, for each `filesystem.read` register `aegis::fs_read` with captured `allowed_root`
- Store capability params in `SandboxState` for host function closure access

**WAT modules** (5 hostile tests using `wat::parse_str`):
1. `allowed_read.wat` — imports `aegis::fs_read`, reads `data/test.txt` (size ≤ limit) → succeeds
2. `denied_read.wat` — reads `../secret.txt` → trap (outside root)
3. `traversal.wat` — reads `../../etc/passwd` → trap (path traversal)
4. `fd_leak.wat` — attempts to use fd 0/1 without capability → trap (no fd table exists)
5. `size_exceeded.wat` — reads `data/large.bin` (size > max_read_bytes) → trap (size limit exceeded)

**Receipt stub**: Emit structured log with `capability: "filesystem.read"`, `path: String`, `size: u64`, `result: Success|Trap|SizeExceeded`. Phase 3 replaces with signed receipts.

**Dependencies**: None new. Uses `std`, `wasmtime`, `serde_json` (already present). `wasi` crate removed from Cargo.toml.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modified | Add `instantiate(wasm_bytes, capabilities)`, host function registration |
| `src/capabilities/mod.rs` | None | Existing `Capability` struct and `builtin::FILESYSTEM_READ` used as-is |
| `src/receipts/mod.rs` | Modified | Add stub `emit_capability_event()` for receipt emission |
| `tests/sandbox.rs` | Modified | Add 5 integration tests with hostile WAT modules |
| `Cargo.toml` | Modified | Remove `wasi` crate (transitive), no new deps |
| `openspec/specs/sandbox-init/spec.md` | None | Phase 0 spec unchanged; Phase 1 adds delta spec |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Path canonicalization bypass (symlinks, mount points) | Medium | Use `std::fs::canonicalize` on both path and allowed_root before comparison |
| Guest memory read/write errors in host function | Low | Validate `path_ptr`/`path_len` and `out_ptr`/`out_len` bounds before access |
| `std::fs::read` blocking on large files | Low | `stat()` size check before read; `max_read_bytes` limit enforced |
| Wasmtime `func_wrap` signature mismatch | Low | Use wasmtime 24.0 typed `Func::wrap` with `Caller<'_, SandboxState>` |
| TOCTOU between `stat()` and `read()` (symlink swap) | Low (Phase 1 single-threaded) | Documented as known limitation; Phase 2/3 will address with `openat` + fd passing or immutable roots |

## Rollback Plan

1. Revert `src/sandbox/mod.rs` to Phase 0 `instantiate(&[u8])` signature
2. Remove host function registration code
3. Re-add `wasi` crate to Cargo.toml if needed for other deps
4. Delete 5 hostile WAT test modules from `tests/sandbox.rs`
5. Remove receipt stub from `src/receipts/mod.rs`
6. `cargo test` passes with Phase 0 tests only

## Dependencies

- None new. Existing: `wasmtime 24.0`, `serde`, `serde_json`, `anyhow`, `wat` (dev)
- `wasi` crate removed (was transitive via wasmtime 24.0)

## Success Criteria

- [ ] `cargo test` passes with 5 new integration tests (all 5 hostile WAT modules)
- [ ] Allowed read (size ≤ limit) returns file contents; denied/traversal/fd-leak/size-exceeded all trap
- [ ] No `wasi` crate in `Cargo.lock` after build
- [ ] Receipt stub logs capability + path + size for each `fs_read` call
- [ ] `Sandbox::instantiate` accepts `&[Capability]` and registers host functions
- [ ] Code compiles with `cargo check` and `cargo clippy -D warnings`
# ADR-010: Host Functions vs WASI

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
Wasmtime supports WASI (WebAssembly System Interface) as a standardized capability set. The aegis project chose to implement custom host functions (`aegis_fs_read`, `aegis_fs_write`) instead of using WASI preview1 or preview2.

### Decision
Use custom host functions via `Linker::func_wrap` instead of WASI.

### Rationale
- **Fine-grained capability control**: WASI provides broad system access (fd_read, fd_write, path_open, etc.) that cannot be easily scoped to a single capability like `filesystem.read` with an `allowed_root`. Custom host functions allow per-capability closures that capture `allowed_root` and `max_bytes` at link time.
- **Security boundary clarity**: Each host function is explicitly named (`aegis_fs_read`, `aegis_fs_write`) and validated against the capability config before any filesystem operation. WASI's flat fd-based model would require additional authorization layers.
- **Path validation integration**: Custom functions embed the full path traversal check (`..` rejection, canonicalize + `starts_with` root check, symlink resolution) directly in the host function closure. WASI would require these checks in a separate policy layer.
- **Receipt emission**: Custom functions emit signed receipts at each validation point (path OOB, traversal, size exceed, success). WASI does not have hooks for receipt emission at the syscall level.
- **Future extensibility**: Custom host functions can be extended to `network.http`, `crypto.sign`, `crypto.verify` without WASI version compatibility constraints.

### Tradeoffs
- Not standardized — custom ABI means guest modules must import `aegis` namespace functions
- WASI modules from the ecosystem cannot run without an adapter layer
- More implementation effort per capability vs. using existing WASI imports

### Consequences
- Guest WASM modules must be compiled with `aegis` imports, not WASI imports (the `aegis_fs_read`/`aegis_fs_write` pattern)
- WASI modules produce linking errors at instantiation (S-4) — this is documented and expected
- The `Capability` enum maps directly to host function registration, creating a tight coupling between capability grants and host function exports

### Traceability
- Design: `src/sandbox/mod.rs` `instantiate_with_capabilities()`, `src/capabilities/mod.rs`
- Spec: All filesystem specs (REQ-101..107, REQ-501..512)
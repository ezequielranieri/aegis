# Design: Phase 1 — Filesystem Read-Only Capability Grant

## Technical Approach

Custom host function `aegis::fs_read` registered via `Linker::func_wrap` when `filesystem.read` capability is present. NO WASI — the runtime decides access, not WASI. The host function validates path against `allowed_root`, rejects `..` traversal, enforces `max_read_bytes` via `stat()`, reads via `std::fs::read`, and traps on ALL violations (fail-closed). Receipt stub logs capability/path/size/result BEFORE any trap. S-4 (FD leak) is a linking error at `Instance::new()` because zero WASI functions are registered.

## Architecture Decisions

### Decision: No WASI, Custom Host Functions Only

| Option | Tradeoff | Decision |
|--------|----------|----------|
| WASI Preview 1 (`wasi` crate) | Already transitive; standard ABI; path resolution in host | **Rejected** — WASI returns errors, we need traps for bypass attempts |
| Custom `func_wrap` only | Complete control; traps on violation; no fd table needed | **Chosen** — matches fail-closed invariant, no WASI dependency |
| WASI + custom validation layer | Hybrid; standard `fd_read` + custom path check | Rejected — adds complexity, two code paths |

**Rationale**: The spec requires traps for ALL violations (REQ-107). WASI Preview 1 returns errno on denied access — not a trap. A custom host function via `func_wrap` gives us exact control: path validation, size check, and `Trap::new()` on any violation. No fd table needed (REQ-105).

### Decision: S-4 = Linking Error, Not Runtime Trap

| Aspect | Detail |
|--------|--------|
| Behavior | Module imports `wasi_snapshot_preview1::fd_read` → `Instance::new()` fails with "unknown import" |
| Test Assertion | `assert!(instance_result.is_err()); assert!(error.to_string().contains("unknown import"));` |
| Why | Zero WASI functions registered in `Linker` — any WASI import fails at instantiation |

### Decision: Receipt Emission BEFORE Trap

| Step | Action |
|------|--------|
| 1 | Validate path/size |
| 2 | **Emit receipt stub** (capability, path, size, result=Success\|Trap\|SizeExceeded) |
| 3 | If violation → `Trap::new()` |
| 4 | If valid → `std::fs::read` → copy to guest memory → return 0 |

**Rationale**: REQ-108 requires logging denied/size-exceeded/traversal attempts. Emitting BEFORE trap ensures S-2, S-3, S-5 are recorded.

### Decision: TOCTOU Documented as Known Limitation

| Scope | Detail |
|-------|--------|
| Phase 1 | Single-threaded, single-root, `stat()` then `read()` race window exists |
| Mitigation | Documented in spec; Phase 2/3 will use `openat` + fd passing or immutable roots |

## Data Flow

```
Guest WASM Module
       │
       │ calls aegis::fs_read(path_ptr, path_len, out_ptr, out_len)
       ▼
┌─────────────────────────────────────────────────────────────┐
│ Host Function: aegis_fs_read (via func_wrap)               │
│   1. Read path string from guest memory (path_ptr/len)     │
│   2. Reject if path contains ".." segment                  │
│   3. Canonicalize path + allowed_root                      │
│   4. Verify path starts with allowed_root                  │
│   5. stat(path) → check size ≤ max_read_bytes              │
│   6. EMIT RECEIPT STUB (before trap/read)                  │
│   7a. If violation → Trap::new("descriptive message")      │
│   7b. If valid → std::fs::read → bounds-check out_len      │
│       → write to guest out_ptr → return 0                   │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
  Returns 0 (success) or Traps (violation)
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `src/sandbox/mod.rs` | Modify | Extend `SandboxState` with `capabilities: Vec<CapabilityConfig>`, add `instantiate(wasm_bytes, capabilities)` overload, register `aegis::fs_read` per capability |
| `src/capabilities/mod.rs` | None | Existing `Capability` struct and `builtin::FILESYSTEM_READ` used as-is |
| `src/receipts/mod.rs` | Modify | Add `emit_capability_event(capability, path, size, result)` stub |
| `tests/sandbox.rs` | Modify | Add 5 integration tests with hostile WAT modules |
| `Cargo.toml` | Modify | Remove `wasi` crate (transitive), no new deps |

## Interfaces / Contracts

### Capability Configuration (captured in SandboxState)

```rust
#[derive(Debug, Clone)]
pub struct CapabilityConfig {
    pub name: String,                    // "filesystem.read"
    pub allowed_root: std::path::PathBuf, // canonicalized absolute path
    pub max_read_bytes: u64,             // default 1_048_576 (1 MB)
}

impl From<&Capability> for CapabilityConfig {
    fn from(cap: &Capability) -> Self {
        let allowed_root = cap.params.get("allowed_root")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/data"));
        let max_read_bytes = cap.params.get("max_read_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(1_048_576);
        Self {
            name: cap.name.clone(),
            allowed_root: std::fs::canonicalize(&allowed_root).unwrap_or(allowed_root),
            max_read_bytes,
        }
    }
}
```

### Extended SandboxState

```rust
#[derive(Default)]
pub struct SandboxState {
    limits: StoreLimits,
    capabilities: Vec<CapabilityConfig>,  // NEW: captured for host function closures
}
```

### Host Function Signature (wasmtime 24.0)

```rust
fn aegis_fs_read(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_len: i32,
) -> i32 {
    // 1. Validate guest memory bounds
    let memory = caller.get_export("memory").and_then(|e| e.into_memory())
        .expect("memory export required");
    
    // 2. Read path string from guest memory
    let path_bytes = memory.data(&caller)
        .get(path_ptr as usize..(path_ptr + path_len) as usize)
        .ok_or_else(|| Trap::new("path_ptr/path_len out of bounds"))?;
    let path = std::str::from_utf8(path_bytes)
        .map_err(|_| Trap::new("invalid UTF-8 in path"))?;

    // 3. Get capability config (single filesystem.read for Phase 1)
    let cap = caller.data().capabilities.iter()
        .find(|c| c.name == "filesystem.read")
        .ok_or_else(|| Trap::new("filesystem.read capability not granted"))?;

    // 4. REQ-103: Reject .. segments BEFORE canonicalize
    if path.contains("..") {
        emit_capability_event(&cap.name, path, 0, "Trap");
        return Trap::new("path traversal attempt: '..' segment detected").into();
    }

    // 5. REQ-102: Canonicalize and verify within allowed_root
    let full_path = std::fs::canonicalize(path)
        .map_err(|_| Trap::new("path canonicalization failed"))?;
    if !full_path.starts_with(&cap.allowed_root) {
        emit_capability_event(&cap.name, path, 0, "Trap");
        return Trap::new("path outside allowed root").into();
    }

    // 6. REQ-104: stat() size check
    let metadata = std::fs::metadata(&full_path)
        .map_err(|_| Trap::new("file metadata read failed"))?;
    let file_size = metadata.len();
    if file_size > cap.max_read_bytes {
        emit_capability_event(&cap.name, path, file_size, "SizeExceeded");
        return Trap::new(format!(
            "file size {} exceeds capability limit {}", 
            file_size, cap.max_read_bytes
        )).into();
    }

    // 7. Emit receipt stub BEFORE read
    emit_capability_event(&cap.name, path, file_size, "Success");

    // 8. Read and copy to guest memory
    let contents = std::fs::read(&full_path)
        .map_err(|_| Trap::new("file read failed"))?;
    
    // Bounds check output buffer
    if contents.len() > out_len as usize {
        return Trap::new("output buffer too small").into();
    }
    memory.data_mut(&mut caller)[out_ptr as usize..(out_ptr + contents.len() as i32) as usize]
        .copy_from_slice(&contents);
    
    0 // success
}
```

### Receipt Stub

```rust
// src/receipts/mod.rs
pub fn emit_capability_event(
    capability: &str,
    path: &str,
    size: u64,
    result: &str,  // "Success" | "Trap" | "SizeExceeded"
) {
    // Phase 1: structured log (Phase 3 replaces with signed receipts)
    tracing::info!(
        capability = capability,
        path = path,
        size = size,
        result = result,
        "capability_event"
    );
}
```

### Sandbox::instantiate Extension

```rust
impl Sandbox {
    pub fn instantiate(
        &mut self, 
        wasm_bytes: &[u8], 
        capabilities: &[Capability]
    ) -> Result<Instance> {
        // Convert capabilities to config for closure capture
        let capability_configs: Vec<CapabilityConfig> = capabilities
            .iter()
            .map(CapabilityConfig::from)
            .collect();

        // Store in SandboxState for host function access
        self.store.data_mut().capabilities = capability_configs.clone();

        // Build linker with host functions per capability
        let mut linker = Linker::new(&self.engine);
        
        for cap in &capability_configs {
            if cap.name == "filesystem.read" {
                let allowed_root = cap.allowed_root.clone();
                let max_read_bytes = cap.max_read_bytes;
                
                linker.func_wrap("aegis", "fs_read", move |caller: Caller<'_, SandboxState>,
                    path_ptr: i32, path_len: i32, out_ptr: i32, out_len: i32| -> i32 {
                    // ... host function body (see above)
                })?;
            }
        }

        let module = Module::new(&self.engine, wasm_bytes)?;
        let instance = Instance::new(&mut self.store, &module, &linker)?;
        Ok(instance)
    }
}
```

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Unit | Path validation logic, size check, traversal rejection | Direct function calls with mocked `SandboxState` |
| Integration | 5 hostile WAT modules (S-1 through S-5) | `wat::parse_str` → `Sandbox::instantiate` → call `_start` → assert trap/success |
| E2E | Receipt emission order (before trap) | Verify log output sequence for denied/traversal/size-exceeded |

### WAT Modules (5 Scenarios)

**S-1: Allowed Read** — imports `aegis::fs_read`, reads file within root, size ≤ limit → returns 0

```wat
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 1024) "test.txt\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 8) (i32.const 2048) (i32.const 1024))
    drop
  )
)
```

**S-2: Denied Read** — path resolves outside `allowed_root` → trap

```wat
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 1024) "../secret.txt\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 13) (i32.const 2048) (i32.const 1024))
    drop
  )
)
```

**S-3: Path Traversal** — path contains `../../etc/passwd` → trap (REQ-103, before canonicalize)

```wat
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 1024) "../../etc/passwd\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 15) (i32.const 2048) (i32.const 1024))
    drop
  )
)
```

**S-4: FD Leak** — imports `wasi_snapshot_preview1::fd_read` → **linking error at `Instance::new()`**

```wat
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (func (export "_start")
    (call $fd_read (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))
    drop
  )
)
```

**S-5: Size Exceeded** — file size > `max_read_bytes` → trap

```wat
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 1024) "large.bin\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 9) (i32.const 2048) (i32.const 1024))
    drop
  )
)
```

## Threat Matrix

| Row | Boundary | Applicable | Expected Safe Behavior | Expected Failure Behavior | RED Test |
|-----|----------|------------|------------------------|---------------------------|----------|
| T-1 | Path traversal (`..`) | **Yes** | Reject at path string level before canonicalize | Trap with "path traversal attempt" | S-3 |
| T-2 | Symlink escape | **Yes** | `canonicalize()` resolves symlinks, verify within root | Trap if resolved path outside root | S-2 (with symlink) |
| T-3 | Size limit bypass | **Yes** | `stat()` before read, trap if > max_read_bytes | Trap with "file size exceeds limit" | S-5 |
| T-4 | FD table / raw fd | **Yes** | No fd table exposed; WASI imports fail at link time | Linking error "unknown import" | S-4 |
| T-5 | TOCTOU (stat→read) | **Yes** | Documented limitation (single-threaded Phase 1) | Race window exists | N/A (Phase 2/3) |
| T-6 | Guest memory bounds | **Yes** | Validate `path_ptr`/`len` and `out_ptr`/`len` before access | Trap on OOB access | Unit test |
| T-7 | VCS/PR automation | No | — | — | — |
| T-8 | Process integration | No | — | — | — |

## Migration / Rollout

No migration required. Phase 1 is additive:
- `Sandbox::instantiate` signature change is backward-compatible (new overload)
- `wasi` crate removed (was transitive only)
- No existing functionality modified

## Open Questions

- [ ] Should `allowed_root` be configurable per-module or global? (Currently per-capability, single filesystem.read)
- [ ] Default `max_read_bytes` = 1 MB — is this the right default for all use cases?
- [ ] Should we add a test for symlink escape (T-2) in Phase 1 or defer to Phase 2?

---

*Design complete. Ready for task breakdown (sdd-tasks).*
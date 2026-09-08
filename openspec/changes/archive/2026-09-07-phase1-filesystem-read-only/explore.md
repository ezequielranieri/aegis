# Exploration: Phase 1 — Filesystem Read-Only Capability Grant

## Current State

The aegis project is a Rust/Wasmtime 24.0 codebase at v0.1.0. **Phase 0 is complete**: `Sandbox::new_with_limits()` provides epoch interruption + `StoreLimitsBuilder` with 4 hard limits + `trap_on_grow_failure(true)`. All resource exhaustion paths produce deterministic traps (fail-closed, REQ-007).

Key current state relevant to Phase 1:

- **`src/sandbox/mod.rs`**: `Sandbox`, `SandboxConfig`, `EpochInterrupter` fully implemented. `instantiate()` and `store_mut()` available. No WASI host functions registered.
- **`src/capabilities/mod.rs`**: `Capability` struct exists (`name: String`, `params: HashMap<String, Value>`), `CapabilityProvider` trait, and `builtin` constants (`FILESYSTEM_READ`, etc.) are defined but **not wired to any host function**.
- **`src/policy/mod.rs`**: `Policy`, `PolicyRule`, `PolicyEngine` exist. `evaluate()` returns `Allow`/`Deny`. **Not yet integrated with sandbox**.
- **`tests/sandbox.rs`**: 4 integration tests for Phase 0 (memory growth, infinite loop, config defaults, epoch interrupter drop, friendly memory growth). Uses `wat::parse_str` for WAT→WASM compilation.
- **No WASI imports exist** anywhere in the source code. The sandbox is bare-metal with zero host exports/imports.
- **`wasi` crate `0.11.1+wasi-snapshot-preview1`** is a **transitive dependency** of wasmtime 24.0 (confirmed in Cargo.lock). This provides WASI Preview 1 support.
- **`wasmtime-wasi` (Preview 2) is NOT in Cargo.lock** — it requires wasmtime 28+.
- **`wasmtime-component-macro` and `wasmtime-wit-bindgen`** exist in wasmtime 24.0 dependencies — Component Model tooling is available at the cranelift level, but no `wasmtime-wasi` equivalent exists for Preview 2 WASI filesystem.

### Dependency Landscape

| Crate | Version | Status | Relevance |
|-------|---------|--------|-----------|
| `wasmtime` | 24.0.13 | Direct dep | Core runtime, `Config::epoch_interruption`, `StoreLimitsBuilder` |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | Transitive dep | WASI Preview 1 — `WasiCtx`, `WasiCtxBuilder`, `Rights`, `PreopenDir` |
| `wasmtime-wasi` | NOT PRESENT | Requires wasmtime 28+ | Preview 2 WASI — unavailable |
| `wasmtime-wit-bindgen` | 24.0.13 | Transitive dep | WIT codegen — exists but no WASI Preview 2 component |
| `wat` | 1.0 | Dev dep | WAT→WASM parsing for integration tests |

---

## Key Decision 1: WASI Preview Version

### Option A: WASI Preview 1 (`wasi` crate) — **RECOMMENDED**

The `wasi` crate (`0.11.1+wasi-snapshot-preview1`) is **already a transitive dependency** of wasmtime 24.0. It provides:
- `WasiCtxBuilder::preopen_dir()` — preopens a directory for filesystem access
- `wasi_snapshot_preview1::fd_read` — standard fd-based read
- `wasi_snapshot_preview1::path_open` — path-based open with rights
- `Rights` struct — granular fd/path rights (`RIGHTS_FD_READ`, `RIGHTS_PATH_READ`)
- Path resolution happens in the **host** — path traversal (`../../etc/passwd`) is caught by `WasiCtx` path validation

**Pros:**
- Already available — no new dependency needed, `wasi` is in the Cargo.lock
- `WasiCtxBuilder::preopen_dir()` enforces read-only via preopened directory boundaries
- FD leak impossible — WASI rights model denies unauthorized fd operations
- Standardized ABI — any WASI Preview 1 module can import `wasi_snapshot_preview1::*`
- Path traversal blocked at host level — WASI resolves paths against preopened roots
- Mature and tested — used in production runtimes for years

**Cons:**
- WASI Preview 1 is legacy — no component model
- Rights management is verbose (need `Rights::FD_READ` + `Rights::PATH_READ`)
- `fd_read` requires fd number from `path_open` first — multi-step API
- The `wasi` crate may pull in additional transitive dependencies

**Effort:** Low-Medium (1-2 days)

### Option B: WASI Preview 2 / Component Model (`wasmtime-wasi`) — NOT VIABLE

`wasmtime-wasi` (Preview 2) version `49.0.0-rc.1` requires wasmtime 28+. wasmtime 24.0 does **not** include this crate. While `wasmtime-component-macro` and `wasmtime-wit-bindgen` exist in wasmtime 24.0, there is no `wasmtime-wasi` package that provides the Preview 2 WASI filesystem interface.

**Pros:**
- Future-proof, component model
- Typed WIT interfaces

**Cons:**
- **Incompatible with wasmtime 24.0** — would require a major version upgrade
- No `wasmtime-wasi` package available for wasmtime 24.x
- Would require migrating all wasmtime imports from 24.x to 28.x

**Effort:** Blocked — requires wasmtime upgrade

### Option C: Custom Host Functions via `func_wrap` — VIABLE but NOT RECOMMENDED

Define `fd_read` and `path_open` as custom wasmtime host functions using `Func::wrap()`. Implement path validation directly in Rust.

**Pros:**
- Complete control over what's exposed
- No WASI fd abstraction overhead
- Path validation entirely in host code
- Simple single-function API

**Cons:**
- Non-standard ABI — modules must import `aegis::fs_read` instead of `wasi_snapshot_preview1::fd_read`
- No fd number management — must implement our own fd table
- No standardized rights model — must implement access control manually
- No WASI module compatibility — only modules compiled against our custom ABI work
- More custom code for fd table, path resolution, rights checking

**Effort:** Medium (2-3 days)

### Option D: Component Model with Custom WIT Interfaces — PARTIALLY VIABLE

Define a WIT interface for filesystem read-only. Use `wasmtime-wit-bindgen` to generate Rust bindings. But without `wasmtime-wasi`, the WASI filesystem component doesn't exist — we'd be building it from scratch.

**Pros:**
- Typed contracts, schema verification
- Future-proof if/when wasmtime 28+ is adopted

**Cons:**
- No `wasmtime-wasi` equivalent for Preview 2 — must implement filesystem component from scratch
- More complex than WASI Preview 1 for the same functionality
- No standard WAT modules can import these interfaces
- Would need custom component linking

**Effort:** High (1+ week)

---

## Key Decision 2: Host Function ABI

### Recommendation: WASI Preview 1 Standard Imports

Use `wasi_snapshot_preview1::fd_read` and `wasi_snapshot_preview1::path_open` as the host function ABI. This is **not** func_wrap and **not** custom WIT — it's the standard WASI Preview 1 import namespace that any compliant WAT module can use.

The WAT module would import:
```wat
(import "wasi_snapshot_preview1" "fd_read" ...)
(import "wasi_snapshot_preview1" "path_open" ...)
```

This means:
1. **No custom ABI** — standardized, well-documented, testable with real WAT
2. **Host-side enforcement** — `WasiCtx` validates paths against preopened directories
3. **Rights enforcement** — `Rights::FD_READ` + `Rights::PATH_READ` restrict to read-only
4. **Path traversal blocked** — `WasiCtx::preopen_dir()` establishes a root; paths outside the root are rejected by the host
5. **FD leak prevented** — fds are only created via `path_open` with explicit rights; no fd passing without capability

---

## Key Decision 3: Capability Policy Struct

The `Capability` struct already exists in `src/capabilities/mod.rs`:

```rust
pub struct Capability {
    pub name: String,
    pub params: HashMap<String, serde_json::Value>,
}
```

For Phase 1, the hardcoded policy maps to this struct as follows:

```rust
/// Hardcoded Phase 1 capability grant: filesystem read-only
pub const FS_READ_CAPABILITY: Capability = Capability {
    name: "filesystem.read".to_string(),
    params: {
        let mut p = HashMap::new();
        p.insert("allowed_root".to_string(), serde_json::Value::String("/data".to_string()));
        p.insert("readonly".to_string(), serde_json::Value::Bool(true));
        p
    },
};
```

**Phase 1 hardcoded policy → Phase 2 policy engine mapping:**
- `params["allowed_root"]` → `PolicyRule.conditions["allowed_root"]`
- `params["readonly"]` → `PolicyRule.conditions["readonly"]`
- `name` → `PolicyRule.capability`
- Current: `Sandbox` hardcodes `FS_READ_CAPABILITY` in `instantiate()`
- Phase 2: `PolicyEngine.evaluate("filesystem.read", context)` replaces hardcoded check

---

## Host Function Signatures (wasmtime 24.0 API)

### WASI `path_open` (for opening files by path)

```rust
// WASI Preview 1 signature via wasi crate
// In WAT, imported as: (import "wasi_snapshot_preview1" "path_open" ...)
//
// Rust side via WasiCtx:
// WasiCtxBuilder::preopen_dir(dir, "data")?  // preopens /data as fd 3
// // Then modules call path_open with this fd
//
// wasmtime 24.0 API:
// let wasi_ctx = WasiCtxBuilder::new()
//     .preopen_dir(std::fs::read_dir("/data")?, "data")?
//     .build();
// store.data_mut().set_wasi_ctx(wasi_ctx);
```

### WASI `fd_read` (for reading from an fd)

```rust
// WASI Preview 1 signature via wasi crate
// In WAT, imported as: (import "wasi_snapshot_preview1" "fd_read" ...)
//
// Rust side: handled automatically by WasiCtx when fd is valid
// The wasi crate implements fd_read in the host
//
// Key: wasmtime 24.0 attaches the WasiCtx to the Store:
// store.data_mut().set_wasi_ctx(wasi_ctx);
```

### How WASI is attached to the Store in wasmtime 24.0

```rust
use wasi::WasiCtxBuilder;
use wasi::WasiCtx;

let wasi_ctx = WasiCtxBuilder::new()
    .preopen_dir("/data", "data")?  // Preopens /data as fd 3, accessible as "data"
    .inherit_stderr()                // Optional: inherit stderr for debugging
    .build();

store.data_mut().set_wasi_ctx(wasi_ctx);
```

**Critical**: `preopen_dir` establishes the filesystem root. The WASI module can only access files within the preopened directory. `../../etc/passwd` resolves outside the root and is rejected by the host.

---

## 4 Hostile WAT Modules for Acceptance Criteria

### Scenario 1: Allowed Read (REQ-AC-1)

```wat
;; allowed_read.wat
;; Reads within the preopened /data directory
(module
  (import "wasi_snapshot_preview1" "fd_read"
    (param i32 i32 i32 i32) (result i32)
    (param i32 i32 i32 i32)
  )
  (import "wasi_snapshot_preview1" "path_open"
    (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
  )
  (import "wasi_snapshot_preview1" "fd_close"
    (param i32) (result i32)
  )
  (import "wasi_snapshot_preview1" "environ_sizes_get"
    (param i32 i32) (result i32)
  )

  (memory 1)
  (data (i32.const 1024) "test.txt\0")
  (data (i32.const 2048) "hello\0")

  ;; Path to open: "data/test.txt" (relative to preopened dir)
  ;; fd 3 is the preopened directory
  (func (export "_start")
    ;; path_open: dirfd=3, path_ptr=1024, path_len=9
    ;; o_flags=0x0 (O_RDONLY), fs_rights_base=0x1 (RIGHTS_FD_READ)
    ;; fs_rights_inheriting=0x1
    (call $path_open
      (i32.const 3)    ;; dirfd = preopened dir
      (i32.const 1024) ;; path pointer
      (i32.const 9)    ;; path length
      (i32.const 0)    ;; o_flags = O_RDONLY
      (i32.const 0x1)  ;; fs_rights_base = RIGHTS_FD_READ
      (i32.const 0x1)  ;; fs_rights_inheriting
      (i32.const 0)    ;; flags = 0
      (i32.const 0)    ;; fdflags = 0
      (i32.const 0)    ;; string_flags = 0
      (i32.const 0)    ;; parent_flags = 0
    )
    drop

    ;; fd_read on the returned fd
    ;; ... (reads file contents into memory)

    (call $fd_close (i32.const 3))
    drop
  )
)
```

**Expected**: `fd_read` returns file contents successfully. No trap.

### Scenario 2: Denied Read (REQ-AC-2)

```wat
;; denied_read.wat
;; Attempts to read from a path outside the preopened directory
(module
  (import "wasi_snapshot_preview1" "path_open"
    (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
  )
  (import "wasi_snapshot_preview1" "fd_read"
    (param i32 i32 i32 i32) (result i32)
    (param i32 i32 i32 i32)
  )
  (import "wasi_snapshot_preview1" "environ_sizes_get"
    (param i32 i32) (result i32)
  )

  (memory 1)
  (data (i32.const 1024) "../secret.txt\0")

  (func (export "_start")
    ;; path_open: attempts to open "../secret.txt" from preopened dir
    ;; This should fail because the path resolves outside the preopened root
    (call $path_open
      (i32.const 3)    ;; dirfd = preopened dir
      (i32.const 1024) ;; path pointer = "../secret.txt"
      (i32.const 13)   ;; path length
      (i32.const 0)    ;; o_flags = O_RDONLY
      (i32.const 0x1)  ;; fs_rights_base = RIGHTS_FD_READ
      (i32.const 0x1)  ;; fs_rights_inheriting
      (i32.const 0)    ;; flags = 0
      (i32.const 0)    ;; fdflags = 0
      (i32.const 0)    ;; string_flags = 0
      (i32.const 0)    ;; parent_flags = 0
    )
    drop

    ;; If path_open succeeds (it shouldn't), fd_read would be called
    ;; If path_open fails, the result is ENOENT or EPERM

    (call $fd_close (i32.const 3))
    drop
  )
)
```

**Expected**: `path_open` returns an error (ENOENT/EPERM) because the host-side WASI context rejects paths outside the preopened directory. **No crash** — graceful WASI error return, not a trap.

Wait — the acceptance criteria says "denied without crash". Let me reconsider: should this be a trap or a WASI error return?

Looking at the acceptance criteria more carefully:
- "Case denied without crash (read outside permitted path)" — this says "without crash"
- A WASI `path_open` returning -1 (ERROR) is the correct fail-closed behavior for this case
- A **trap** would be for the bypass scenarios
- Actually, re-reading: "denied without crash" means the module should NOT crash, and the read should be denied. This is a WASI error return, not a trap.

But wait — the fail-closed invariant (REQ-007) says ALL resource exhaustion = trap. This is different — it's an access control denial, not resource exhaustion. WASI `path_open` returns an error code (not a trap) for permission denied. This is consistent.

However, looking at the original Phase 1 scope again: "Caso denegado sin crash (read fuera de path permitido)" — this means the read is denied, no crash. The WASI context will return an error. The module continues execution. This is correct behavior.

For the **bypass** scenarios (path traversal and fd leak), the expectation is a **trap** (fail-closed).

### Scenario 3: Bypass — Path Traversal (REQ-AC-3)

```wat
;; bypass_traversal.wat
;; Attempts path traversal to read /etc/passwd via ../../etc/passwd
(module
  (import "wasi_snapshot_preview1" "path_open"
    (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
  )
  (import "wasi_snapshot_preview1" "fd_read"
    (param i32 i32 i32 i32) (result i32)
    (param i32 i32 i32 i32)
  )
  (import "wasi_snapshot_preview1" "environ_sizes_get"
    (param i32 i32) (result i32)
  )

  (memory 1)
  (data (i32.const 1024) "../../etc/passwd\0")

  (func (export "_start")
    ;; path_open with traversal path — WASI host-side path resolution
    ;; SHOULD trap because the path resolves outside the preopened root
    ;; WASI may return an error or the host may trap depending on configuration
    (call $path_open
      (i32.const 3)    ;; dirfd = preopened dir
      (i32.const 1024) ;; path = "../../etc/passwd"
      (i32.const 15)   ;; path length
      (i32.const 0)
      (i32.const 0x1)
      (i32.const 0x1)
      (i32.const 0)
      (i32.const 0)
      (i32.const 0)
      (i32.const 0)
    )
    drop

    (call $fd_close (i32.const 3))
    drop
  )
)
```

**Expected**: WASI `path_open` rejects `../../etc/passwd` because the host-side `WasiCtx` resolves paths against the preopened root directory. The path `../../etc/passwd` escapes the root and the host returns an error. **No crash** — WASI error return.

Actually, this is the same as Scenario 2 in terms of WASI behavior. The distinction is:
- Scenario 2: path outside root → WASI error return (graceful)
- Scenario 3: `../../etc/passwd` → same WASI error return (graceful)

For these to **trap**, we need a different mechanism. The question is: should path traversal trap?

Re-reading the acceptance criteria: "Intento explícito de bypass: path traversal (`../../etc/passwd`) o fd heredado"

And looking at the task description: "Bypass: Path Traversal → Trap (path resolution in host, not guest)"

This means the **host** does the path resolution. If the path traversal is detected by the host, it should trap. This is the fail-closed behavior.

The implementation approach: Instead of relying on WASI's built-in path resolution (which returns errors), we should implement a **custom host function** that does strict path validation and traps on any path that attempts traversal. This means using `func_wrap` for the path validation layer, even if `fd_read` uses WASI.

Alternatively, we can configure `WasiCtx` to trap on path resolution failures instead of returning errors. But I'm not sure `WasiCtx` supports this directly.

**Revised approach for Scenario 3**: Implement a custom host function that wraps `path_open` and does strict path validation. If the path contains `..` or resolves outside the allowed root, the host function **traps** instead of returning an error.

### Scenario 4: Bypass — FD Leak (REQ-AC-4)

```wat
;; bypass_fd_leak.wat
;; Attempts to use a file descriptor inherited from the host without explicit capability
(module
  (import "wasi_snapshot_preview1" "fd_read"
    (param i32 i32 i32 i32) (result i32)
    (param i32 i32 i32 i32)
  )
  (import "wasi_snapshot_preview1" "fd_close"
    (param i32) (result i32)
  )
  (import "wasi_snapshot_preview1" "environ_sizes_get"
    (param i32 i32) (result i32)
  )

  (memory 1)

  (func (export "_start")
    ;; Attempt to fd_read on fd 0 (stdin) or fd 1 (stdout)
    ;; These are inherited from the host but NOT in our preopened dirs
    ;; WASI should reject this because the fd doesn't have READ rights
    ;; OR we can ensure these fds don't exist in our WasiCtx
    (call $fd_read
      (i32.const 0)    ;; fd = 0 (stdin) — not preopened, should fail
      (i32.const 0)    ;; iovs pointer (null)
      (i32.const 0)    ;; iovs_len
      (i32.const 0)    ;; nread pointer
    )
    drop

    (call $fd_close (i32.const 0))
    drop
  )
)
```

**Expected**: `fd_read` on fd 0 fails because the `WasiCtx` doesn't have fd 0 as a valid preopened directory fd. The `WasiCtx` only has preopened directories — stdin/stdout/stderr may or may not be inherited. If configured with `.inherit_stderr()` only, fd 0 won't exist. The host rejects the operation.

For a **trap** scenario: if the module somehow obtains an fd it shouldn't have (e.g., by guessing an fd number), the WASI context should reject it. To make this trap, we need to ensure the WASI context is configured to **only** have preopened dirs and no inherited fds.

---

## Revised Architecture Decision

After analyzing all 4 scenarios, I realize there's a nuance:

- **Scenarios 1** (allowed read) and **2** (denied read) can use standard WASI Preview 1
- **Scenarios 3** (path traversal → trap) and **4** (fd leak → trap) require **strict host-side enforcement**

The WASI `path_open` with `WasiCtx::preopen_dir()` returns errors for paths outside the root (Scenario 2 and 3). This is **not** a trap — it's a WASI error return.

To achieve **traps** for bypass attempts, we need a **custom host function layer** that wraps WASI calls and traps on suspicious paths.

### Recommended Hybrid Approach

1. **Use `WasiCtx` with `preopen_dir`** for standard filesystem access (Scenarios 1 and 2)
2. **Add a custom host function** `aegis::validate_path` that:
   - Checks if the path contains `..` segments
   - Checks if the resolved path is within the allowed root
   - **Traps** if the path is a traversal attempt
3. **Configure `WasiCtx`** to NOT inherit stdin/stdout/stderr (only preopened dirs)
4. **FD leak prevention**: The `WasiCtx` only has preopened directory fds. Any other fd number returns an error or traps.

Wait, actually, let me re-read the task more carefully:

"Caso denegado sin crash (read fuera de path permitido)" — denied without crash
"Intento explícito de bypass: path traversal (`../../etc/passwd`) o fd heredado" — bypass attempts

And: "Bypass: Path Traversal → Trap (path resolution in host, not guest)"

So:
- Scenario 2 (denied): No crash — WASI returns error gracefully
- Scenario 3 (path traversal): **Trap** — the host detects and traps
- Scenario 4 (fd leak): **Trap** — the host detects and traps

The key insight: **path traversal should TRAP** because it's an explicit bypass attempt. This means we need a custom validation layer.

But WASI `path_open` with `preopen_dir` already prevents path traversal. The question is whether it returns an error or traps. WASI returns `-1` with an errno — it doesn't trap.

So for Scenario 3 to trap, we need either:
a) A custom host function that does strict validation and traps
b) Or configure WASI to trap on path resolution failures

Option (a) is more practical. Let me revise the recommendation.

### Final Recommendation: WASI Preview 1 + Custom Path Validation Host Function

Use WASI Preview 1 for the standard `fd_read` ABI, but add a **custom host function** `aegis::fs_read` that:
1. Takes a path string parameter
2. Validates the path against traversal patterns (`..`, absolute paths)
3. If valid: delegates to `WasiCtx::path_open` + `fd_read`
4. If invalid: **traps** (fail-closed)
5. FD leak: the custom function only creates fds through the validated path, preventing fd inheritance

This gives us:
- Standard WASI fd_read for the actual read operation
- Custom validation that traps on bypass attempts
- Clean separation between "denied" (WASI error) and "bypass attempt" (trap)

Actually, looking at this more carefully, I think the simplest approach that satisfies all acceptance criteria is:

**Approach**: Use `WasiCtx` with `preopen_dir` and `RIGHTS_FD_READ` / `RIGHTS_PATH_READ`. Then:
- Scenario 1: `path_open` succeeds → `fd_read` succeeds → **allowed**
- Scenario 2: `path_open` fails with ERROR → **denied without crash** (WASI error)
- Scenario 3: `path_open` fails with ERROR → **but we want TRAP** → need custom validation
- Scenario 4: `fd_read` on non-existent fd fails → **but we want TRAP** → need strict fd validation

For the trap-on-bypass requirement, the cleanest approach is:

**Custom host function `aegis::fs_read`** that:
- Accepts a path parameter
- Validates path: must not contain `..`, must be within allowed root
- If valid: opens file via WASI and reads → returns content
- If invalid: **traps** (using `wasmtime::Trap::new()` or similar)
- No fd passing — reads happen through the function, not via fd numbers

This means the WAT module imports `aegis::fs_read` instead of `wasi_snapshot_preview1::fd_read`. The host function handles all path validation and trapping.

But this contradicts the "standard WASI" approach...

Let me reconsider. The task says to present tradeoffs and make recommendations. Let me present both approaches clearly.

---

## Final Recommendation

### Primary Recommendation: WASI Preview 1 + Custom Bypass Validation Layer

**Two-layer architecture**:

**Layer 1 — WASI Preview 1 standard access**: `WasiCtx` with `preopen_dir("/data")` and `Rights::FD_READ` + `Rights::PATH_READ`. Handles:
- Allowed reads (Scenario 1): succeeds
- Denied reads (Scenario 2): WASI returns error, no crash

**Layer 2 — Custom host function for bypass detection**: `aegis::fs_read(path: &str) -> Result<Vec<u8>, Trap>`. Handles:
- Path traversal detection (Scenario 3): validates path in Rust, traps if `..` detected
- FD leak prevention (Scenario 4): no fd passing; reads go through this function only

**Why not just WASI Preview 1 alone**: WASI returns errors for denied paths (Scenarios 2 and 3). The task explicitly requires **traps** for bypass attempts (Scenario 3). WASI's error-return semantics don't meet this requirement without a custom validation layer.

**Why not just func_wrap alone**: Losing WASI standardization and the ability to test with real WAT modules. The WASI layer provides the `fd_read` infrastructure; the custom layer adds the trap-on-bypass behavior.

---

## Open Questions

1. **How does `WasiCtx::preopen_dir` handle `../../etc/passwd`?** Does it return an error or could it potentially resolve the path? Need to verify the WASI implementation's path resolution semantics. If it returns an error, we still need a custom function to make it trap.

2. **Does `WasiCtx` support trapping on path resolution failures instead of returning errors?** If so, we could configure this instead of adding a custom layer.

3. **What is the exact `wasmtime_wasi` API for wasmtime 24.0?** The `wasi` crate API may differ from `wasmtime_wasi`. Need to verify the exact imports and types.

4. **How does `WasiCtx` handle fd 0/1/2?** By default, WASI may inherit stdin/stdout/stderr. We need to configure the `WasiCtxBuilder` to NOT inherit these, or explicitly deny rights on them.

5. **The `wasi` crate version `0.11.1+wasi-snapshot-preview1` — is it the right crate to import in our code?** Need to verify the actual import path (`use wasi::WasiCtxBuilder` vs `use wasmtime_wasi::WasiCtxBuilder`).

6. **How does the `Capability` struct map to the `WasiCtx` configuration?** The `params["allowed_root"]` should configure `WasiCtxBuilder::preopen_dir`. The `params["readonly"]` should restrict rights to read-only. This mapping needs to be implemented in `Sandbox::instantiate()`.

7. **Phase 0 epoch timer interaction with Phase 1 host functions**: Host functions (WASI) provide natural epoch check points. This resolves the AD-002 limitation about tight loops without function calls. When the WASI module calls `fd_read`, the epoch is checked.

8. **The `Sandbox::instantiate()` method doesn't currently accept capabilities**. It needs to be extended to accept a `Vec<Capability>` and configure the `WasiCtx` accordingly.

---

## Integration with Phase 0 Artifacts

### `Sandbox::new_with_limits()` — No Changes Needed

The sandbox constructor remains unchanged. The WASI context is attached to the Store during `instantiate()`, not during construction.

### `Sandbox::instantiate()` — Extension Needed

```rust
pub fn instantiate(&mut self, wasm_bytes: &[u8], capabilities: &[Capability]) -> Result<Instance> {
    let module = Module::new(&self.engine, wasm_bytes)?;
    
    // Configure WasiCtx based on capabilities
    let wasi_ctx = self.build_wasi_ctx(capabilities)?;
    self.store.data_mut().set_wasi_ctx(wasi_ctx);
    
    let instance = Instance::new(&mut self.store, &module, &[])?;
    Ok(instance)
}
```

### `SandboxConfig` — No Changes Needed (for Phase 1)

The sandbox config remains the same. Capability-specific configuration (allowed_root, etc.) comes from the `Capability` struct passed to `instantiate()`.

### Epoch Interrupter — No Changes Needed

Host function calls (WASI `fd_read`, `path_open`) provide natural epoch check points, resolving the AD-002 limitation. No timer thread changes needed.

### `EpochInterrupter` — No Changes Needed

The mpsc channel design (AD-003) continues to work. No changes.

### `Sandbox::new_with_config(config, enable_epoch)` — No Changes Needed

The test isolation mechanism (AD-004) continues to work. No changes.

---

## Hardcoded Policy → Phase 2 Policy Engine Mapping

### Phase 1 (Current): Hardcoded Allowlist

```rust
// In Sandbox::instantiate() or a new configure_capabilities() method:
let fs_read_cap = Capability {
    name: "filesystem.read".to_string(),
    params: hashmap! {
        "allowed_root".to_string() => serde_json::Value::String("/data".to_string()),
        "readonly".to_string() => serde_json::Value::Bool(true),
    },
};
let capabilities = vec![fs_read_cap];
```

### Phase 2 (Future): Policy Engine Evaluation

```rust
// The PolicyEngine.evaluate() call replaces the hardcoded check:
let effect = policy_engine.evaluate("filesystem.read", &context);
// context includes: path, tenant_id, timestamp, etc.
// If effect == PolicyEffect::Allow → configure WasiCtx
// If effect == PolicyEffect::Deny → skip WASI setup, module runs without FS access
```

### Mapping Table

| Phase 1 (Hardcoded) | Phase 2 (Policy Engine) |
|---------------------|------------------------|
| `Capability.name = "filesystem.read"` | `PolicyRule.capability = "filesystem.read"` |
| `params["allowed_root"] = "/data"` | `PolicyRule.conditions["allowed_root"] = "/data"` |
| `params["readonly"] = true` | `PolicyRule.conditions["readonly"] = true` |
| `Sandbox::instantiate()` hardcodes capability | `PolicyEngine.evaluate()` determines access |
| Default: allow if capability present | Default: deny (PolicyEngine::default_effect) |

---

## Risks

1. **WASI Preview 1 deprecation**: WASI Preview 1 is legacy. Future wasmtime versions may drop support. Mitigation: The `Capability` struct and `WasiCtx` configuration are abstracted enough that swapping the WASI layer is a local change.

2. **`wasi` crate transitive dependency may change**: The `wasi 0.11.1+wasi-snapshot-preview1` is a transitive dep of wasmtime 24.0. If wasmtime upgrades, this could change. Mitigation: Add `wasi` as a direct dependency with a pinned version.

3. **Path resolution semantics of `WasiCtx::preopen_dir`**: Need to verify that `../../etc/passwd` is properly rejected. If the WASI implementation has a bug or different behavior, the bypass test may not work as expected.

4. **Custom trap on bypass**: The `func_wrap` approach for bypass detection requires careful implementation. Trapping a WASM module from the host side must use the wasmtime `Trap` API correctly.

5. **WASI fd inheritance**: If `WasiCtxBuilder` inherits stdin/stdout/stderr by default, fd leak scenarios may not work as expected. Need to explicitly configure no fd inheritance.

6. **WAT module compatibility**: The WAT modules must import `wasi_snapshot_preview1::*` functions. This requires correct WASI function signatures in the WAT code. The wasmtime 24.0 WASI function signatures may differ from the standard documented signatures.

---

## Ready for Proposal

**Yes** — The exploration has identified the WASI Preview 1 approach as the most viable given the wasmtime 24.0 constraint, with a custom bypass validation layer for trap-on-bypass behavior. The `Capability` struct, `PolicyEngine`, and `Sandbox` infrastructure are already in place and need extension, not replacement.

The next step is to create a proposal (`sdd-propose`) that details the implementation plan for:
1. Adding `wasi` as a direct dependency
2. Implementing `Sandbox::configure_capabilities()` with `WasiCtxBuilder`
3. Implementing the custom `aegis::fs_read` host function for bypass detection
4. Writing 4 hostile WAT test modules
5. Extending `Sandbox::instantiate()` to accept capabilities

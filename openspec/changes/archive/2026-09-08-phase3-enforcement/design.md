# Technical Design: Phase 3 — Enforcement (filesystem.write)

## 1. Architecture Overview

### 1.1 Component Relationships

```
┌─────────────────────────────────────────────────────────────────┐
│                        Sandbox                                    │
│  ┌──────────────┐  ┌──────────────────────────────────────────┐  │
│  │ SandboxState │  │ Linker (host functions per capability)   │  │
│  │ - limits     │  │  ┌────────────────────────────────────┐  │  │
│  │ - capabilities: Vec<CapabilityConfig>                   │  │  │
│  │ - receipt_emitter: Option<ReceiptEmitter> ─────────────►  │  │
│  └──────────────┘  │  │ aegis::fs_read  (FilesystemRead)   │  │  │
│                    │  │ aegis::fs_write (FilesystemWrite)  │  │  │
│                    │  └────────────────────────────────────┘  │  │
└────────────────────┴──────────────────────────────────────────┘
         ▲                       ▲                        ▲
         │                       │                        │
         │          ┌────────────┴────────────┐          │
         │          ▼                         ▼          │
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│  CapabilityConfig│  │  ReceiptEmitter  │  │  ExecutionReceipt│
│  - name          │  │  - key_pair      │  │  - capability_name│
│  - allowed_root  │  │  - last_prev_hash│  │  - action        │
│  - max_bytes     │  │  - receipts: Vec │  │  - result        │
└──────────────────┘  │  - force_signing_failure │  - path        │
                      └──────────────────┘     │  - size        │
                                               │  - prev_hash   │
                                               │  - signature   │
                                               │  - timestamp_ns│
                                               └──────────────────┘
```

### 1.2 Integration Points

| Component | Role | Phase 3 Changes |
|-----------|------|-----------------|
| `Sandbox::instantiate_with_capabilities()` | Registers host functions per capability variant | Add `FilesystemWrite` branch registering `aegis::fs_write` |
| `CapabilityConfig` | Captures capability params for closure capture | **Unit A**: Rename `max_read_bytes` → `max_bytes`; update `From<&Capability>` for both read/write |
| `aegis_fs_read` | Read-only host function (template) | **Unit A**: Update to read `cap.max_bytes` |
| `aegis_fs_write` | **New** — Atomic write host function | **Unit B**: Full implementation mirroring `aegis_fs_read` + atomic write |
| `ReceiptEmitter` | Signs and chains receipts | No change — already supports all patterns |
| `SandboxState.receipt_emitter` | Optional emitter in store data | No change — accessed via `caller.data_mut().receipt_emitter` |

### 1.3 CapabilityConfig Field Rename (Unit A)

| Before | After | Rationale |
|--------|-------|-----------|
| `max_read_bytes: u64` | `max_bytes: u64` | Generalize across all capability variants (read, write, network) |

The `From<&Capability>` impl maps:
- `FilesystemRead.params.max_read_bytes` → `max_bytes`
- `FilesystemWrite.params.max_write_bytes` → `max_bytes`
- `NetworkHttp.params.max_requests_per_second` → `max_bytes` (unchanged, already repurposed)

---

## 2. Data Flow: `aegis_fs_write`

```
Guest Memory (WASM)
    │
    ├─ path_ptr, path_len ──────────────► Read path string from guest memory
    │                                         │
    │                                         ▼
    ├─ data_ptr, data_len ─────────────► Read data buffer from guest memory
    │                                         │
    │                                         ▼
    ▼                              ┌───────────────────────┐
┌─────────────────────────────────►│  1. Validate path     │
│                                   │  - no ".." segments   │
│                                   │  - canonicalize       │
│                                   │  - starts with root   │
│                                   │  - symlink escape     │
│                                   └───────────┬───────────┘
│                                               │
│                                               ▼
│                                   ┌───────────────────────┐
│                                   │  2. Validate size     │
│                                   │  - data_len ≤ max_bytes│
│                                   └───────────┬───────────┘
│                                               │
│                                               ▼
│                                   ┌───────────────────────┐
│                                   │  3. Create temp file  │
│                                   │  - target.aegis_tmp   │
│                                   │  - in allowed_root    │
│                                   │  - write data         │
│                                   └───────────┬───────────┘
│                                               │
│                                               ▼
│                                   ┌───────────────────────┐
│                                   │  4. Emit SUCCESS      │
│                                   │  - capability="filesystem.write"│
│                                   │  - action="write"     │
│                                   │  - result="success"   │
│                                   └───────────┬───────────┘
│                                               │
│                          ┌────────────────────┴────────────────────┐
│                          ▼                                         ▼
│                   ┌─────────────┐                          ┌─────────────┐
│                   │ emit OK     │                          │ emit FAILS  │
│                   └──────┬──────┘                          └──────┬──────┘
│                          │                                         │
│                          ▼                                         ▼
│                   ┌─────────────┐                          ┌─────────────┐
│                   │ 5. Rename   │                          │ 5a. Delete  │
│                   │ temp→target │                          │ temp file   │
│                   └──────┬──────┘                          └──────┬──────┘
│                          │                                         │
│               ┌──────────┴──────────┐                             ▼
│               ▼                     ▼                    ┌─────────────┐
│        ┌─────────────┐     ┌─────────────┐               │ 5b. TRAP    │
│        │ rename OK   │     │ rename FAIL │               │ immediately │
│        └──────┬──────┘     └──────┬──────┘               └─────────────┘
│               │                   │
│               ▼                   ▼
│        ┌─────────────┐     ┌─────────────┐
│        │ Return 0    │     │ 7a. Delete  │
│        │ (success)   │     │ temp file   │
│        └─────────────┘     └──────┬──────┘
│                                  │
│                                  ▼
│                         ┌─────────────┐
│                         │ 7b. TRAP    │
│                         └─────────────┘
```

**Critical invariant**: At ALL failure points after temp file creation, the temp file is deleted BEFORE returning Trap.

---

## 3. File Changes

| File | Change Type | Details |
|------|-------------|---------|
| `src/sandbox/mod.rs` | Modified | Unit A: Rename `max_read_bytes` → `max_bytes` in `CapabilityConfig`; update `From<&Capability>`; update `aegis_fs_read` to use `cap.max_bytes`. Unit B: Add `aegis_fs_write` function; register in `instantiate_with_capabilities()` for `FilesystemWrite` variant. |
| `tests/sandbox.rs` | Modified | Add `ALLOWED_WRITE`, `DENIED_WRITE`, `TRAVERSAL_WRITE`, `SIZE_EXCEEDED_WRITE`, `GUEST_OOB_WRITE`, `SYMLINK_WRITE` WAT modules; add test functions `allowed_write_succeeds`, `denied_write_traps`, `traversal_write_traps`, `size_exceeded_write_traps`, `guest_memory_oob_write`, `symlink_escape_write_traps`, `receipt_s416_write_signing_failure`, `receipt_s416_write_after_temp_write`. |
| `tests/fixtures/config/filesystem-write.toml` | New | TOML fixture for `Sandbox::from_config` integration test. |

---

## 4. Unit A Refactor Details

### 4.1 CapabilityConfig Struct Change

```rust
// BEFORE
#[derive(Debug, Clone)]
pub struct CapabilityConfig {
    pub name: String,
    pub allowed_root: PathBuf,
    pub max_read_bytes: u64,  // RENAMED
}

// AFTER
#[derive(Debug, Clone)]
pub struct CapabilityConfig {
    pub name: String,
    pub allowed_root: PathBuf,
    pub max_bytes: u64,       // Generalized field name
}
```

### 4.2 From<&Capability> Implementation Update

```rust
impl From<&Capability> for CapabilityConfig {
    fn from(cap: &Capability) -> Self {
        match cap {
            Capability::FilesystemRead(params) => Self {
                name: "filesystem.read".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_bytes: params.max_read_bytes,  // WAS: max_read_bytes
            },
            Capability::FilesystemWrite(params) => Self {
                name: "filesystem.write".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_bytes: params.max_write_bytes,  // NEW: maps write param to max_bytes
            },
            Capability::NetworkHttp(params) => Self {
                name: "network.http".to_string(),
                allowed_root: PathBuf::new(),
                max_bytes: params.max_requests_per_second,  // Already repurposed
            },
        }
    }
}
```

### 4.3 Call Site Updates in `aegis_fs_read`

```rust
// BEFORE
if file_size > cap.max_read_bytes { ... }

// AFTER
if file_size > cap.max_bytes { ... }
```

### 4.4 Regression Gate (A.7)

**Hard requirement**: `cargo test` must pass with **zero failures** (all 71 existing tests) before any Unit B work begins.

---

## 5. Unit B Implementation: `aegis_fs_write`

### 5.1 Function Signature

```rust
fn aegis_fs_write(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    data_ptr: i32,
    data_len: i32,
) -> Result<i32>
```

Registered via:
```rust
Capability::FilesystemWrite(_) => {
    linker.func_wrap(
        "aegis",
        "fs_write",
        move |caller: Caller<'_, SandboxState>,
              path_ptr: i32,
              path_len: i32,
              data_ptr: i32,
              data_len: i32|
              -> Result<i32> {
            aegis_fs_write(caller, path_ptr, path_len, data_ptr, data_len)
        },
    )?;
}
```

### 5.2 Pseudocode with All 6 Receipt Emission Points

```rust
fn aegis_fs_write(
    mut caller: Caller<'_, SandboxState>,
    path_ptr: i32,
    path_len: i32,
    data_ptr: i32,
    data_len: i32,
) -> Result<i32> {
    // 1. Get capability config FIRST
    let cap = caller
        .data()
        .capabilities
        .iter()
        .find(|c| c.name == "filesystem.write")
        .ok_or_else(|| anyhow::anyhow!("filesystem.write capability not granted"))?
        .clone();
    let cap_name = cap.name.clone();

    // 2. Validate guest memory bounds for PATH (T-6-W)
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("memory export required"))?;

    // 3. Read path string from guest memory
    let path_result = {
        let data = memory.data(&caller);
        data.get(path_ptr as usize..(path_ptr + path_len) as usize)
            .map(|bytes| bytes.to_vec())
    };

    let path_bytes = path_result.ok_or_else(|| {
        // S-5-W: Guest memory OOB on path — emit trap receipt, then trap
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            if let Err(e) = emitter.emit(&cap_name, "write", "", 0, "trap") {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("path_ptr/path_len out of bounds")
    })?;
    let path = std::str::from_utf8(&path_bytes)
        .map_err(|_| anyhow::anyhow!("invalid UTF-8 in path"))?;

    // 4. REQ-503: Reject .. segments BEFORE canonicalize (S-3-W, T-1-W)
    if path.contains("..") {
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            if let Err(e) = emitter.emit(&cap_name, "write", path, 0, "trap") {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path traversal attempt: '..' segment detected");
    }

    // 5. REQ-502: Canonicalize and verify within allowed_root (S-2-W, Symlink-W)
    let full_path = cap.allowed_root.join(&path);
    let canonical_path = std::fs::canonicalize(&full_path)
        .map_err(|_| anyhow::anyhow!("path canonicalization failed"))?;
    let canonical_root = std::fs::canonicalize(&cap.allowed_root)
        .map_err(|_| anyhow::anyhow!("allowed_root canonicalization failed"))?;
    if !canonical_path.starts_with(&canonical_root) {
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            if let Err(e) = emitter.emit(&cap_name, "write", path, 0, "trap") {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("path outside allowed root");
    }

    // 6. Validate guest memory bounds for DATA (T-6-W)
    let data_result = {
        let data = memory.data(&caller);
        data.get(data_ptr as usize..(data_ptr + data_len) as usize)
            .map(|bytes| bytes.to_vec())
    };

    let write_data = data_result.ok_or_else(|| {
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            if let Err(e) = emitter.emit(&cap_name, "write", path, 0, "trap") {
                return anyhow::anyhow!("receipt emission failed: {}", e);
            }
        }
        anyhow::anyhow!("data_ptr/data_len out of bounds")
    })?;

    // 7. REQ-504: Size enforcement (S-5-W)
    let data_size = write_data.len() as u64;
    if data_size > cap.max_bytes {
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            if let Err(e) = emitter.emit(&cap_name, "write", path, data_size, "trap") {
                return Err(anyhow::anyhow!("receipt emission failed: {}", e));
            }
        }
        bail!("data size {} exceeds capability limit {}", data_size, cap.max_bytes);
    }

    // 8. Create temp file: target.aegis_tmp in allowed_root
    let temp_path = canonical_path.with_extension(
        canonical_path.extension().unwrap_or_default().to_string_lossy().to_string() + ".aegis_tmp"
    );
    // If no extension, use "filename.aegis_tmp"
    let temp_path = if temp_path == canonical_path {
        canonical_path.with_file_name(
            canonical_path.file_name().unwrap().to_string_lossy().to_string() + ".aegis_tmp"
        )
    } else {
        temp_path
    };

    // Write data to temp file
    std::fs::write(&temp_path, &write_data)
        .map_err(|_| anyhow::anyhow!("temp file write failed"))?;

    // 9. Atomic rename: temp → target (REQ-505, REQ-506: overwrite allowed)
    // CRITICAL: Rename BEFORE success receipt emission.
    // If rename fails, we can still emit trap and cleanup — no success receipt exists yet.
    if let Err(e) = std::fs::rename(&temp_path, &canonical_path) {
        // Rename failed — cleanup temp file, emit trap receipt, then trap
        let _ = std::fs::remove_file(&temp_path);
        if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
            let _ = emitter.emit(&cap_name, "write", path, data_size, "trap");
        }
        bail!("atomic rename failed: {}", e);
    }

    // 10. Rename succeeded — file is now visible at target path.
    // NOW emit success receipt. This is the critical asymmetry vs aegis_fs_read:
    // In read, we could trap and not return data. In write, the file is ALREADY
    // written to the host filesystem and visible to other processes. We cannot
    // "un-write" it. If emit fails HERE, we have a real write without a receipt.
    if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
        if let Err(e) = emitter.emit(&cap_name, "write", path, data_size, "success") {
            // S-416-W-after-rename: emit failed AFTER successful rename
            // File is already written at target — we CANNOT undo it.
            // This is a known asymmetry vs read. Log/record for audit, then trap.
            // The receipt chain has a gap: write happened but no receipt was added.
            tracing::error!(
                "FAIL-CLOSED VIOLATION: filesystem.write succeeded (file at {}) but receipt emission failed: {}. \
                Host has a write with no signed receipt in chain.",
                canonical_path.display(), e
            );
            // Still return Trap to maintain fail-closed at API level
            return Err(anyhow::anyhow!("receipt emission failed after successful write: {}", e));
        }
    }

    // 11. Success — file written AND receipt emitted
    Ok(0)
}
```

---

## 6. Critical: S-416-W-after-rename Pseudocode (Explicit Step-by-Step)

This is the **critical fail-closed sequence** when receipt emission fails AFTER the atomic rename has succeeded. This is the genuine asymmetry vs `aegis_fs_read` — the file is already written and visible; we cannot undo it.

```
S-416-W-after-rename: Signing Failure After Successful Rename
──────────────────────────────────────────────────────────────────────

1. Validate destination path (canonicalize, root check, symlink, .. rejection)
   → All path validations pass

2. Check size against max_bytes
   → data_len ≤ max_bytes, validation passes

3. Create temp file: target.aegis_tmp in allowed_root
   → temp_path = canonical_path.with_extension(... + ".aegis_tmp")
   → std::fs::write(temp_path, data) succeeds

4. Atomic rename: std::fs::rename(temp_path, canonical_path)
   → RENAME SUCCEEDS
   → File is now visible at target path in allowed_root
   → Data is committed to host filesystem, visible to other processes

5. Emit success receipt (capability="filesystem.write", action="write", result="success")
   → ReceiptEmitter::emit() called with success result
   → SIGNING FAILS (force_signing_failure or actual crypto error)

6. IF emit fails:
   6a. LOG FAIL-CLOSED VIOLATION with full context:
       → "filesystem.write succeeded (file at <path>) but receipt emission failed"
       → Include: canonical path, data size, error details
   6b. Return Trap immediately
       → Host function returns Trap at API level
       → BUT: file IS written on host filesystem (cannot be undone)
       → Receipt chain has a gap: real write occurred, no receipt added

7. Return success (0)  ← NEVER REACHED if emit fails
```

**This is a deliberate, documented deviation from Decision 4's fail-closed pattern.**
In `aegis_fs_read`, fail-closed means "no data returned to guest" — we control the guest memory.
In `aegis_fs_write`, the host filesystem write is a side effect we cannot retract once `rename` succeeds.
The design chooses: **trap at API level + explicit audit log** rather than blocking the write entirely.

**Partial mitigation: idempotent retry is safe.** REQ-506 allows overwrite semantics (`std::fs::write` behavior). If a caller receives Trap after S-416-W-after-rename and retries the write, it will overwrite the same file with the same data — no duplication, no corruption, no state divergence. The functional blast radius is contained to the receipt chain gap; the filesystem state is convergent. This does NOT close the audit gap, but bounds the operational impact.
This is documented as a known limitation (see §7).

---

## 7. Crash Between Temp Write and Rename — Known Limitation

### 7.1 The Gap

If the process crashes (power loss, SIGKILL, panic) **after step 3** (temp file written) but **before step 4** (rename completes), the `.aegis_tmp` file remains on disk.

### 7.2 Why This Is Acceptable for Phase 3

| Aspect | Assessment |
|--------|------------|
| Atomic rename guarantee | `std::fs::rename` is atomic within a filesystem — target is either old file or new file, never partial |
| Temp file persistence | `.aegis_tmp` is an orphaned file in `allowed_root`, not exposed to WASM |
| Security impact | None — temp file is inside `allowed_root`, contains valid data, no path escape |
| Comparison | Equivalent to TOCTOU in Phase 1 `aegis_fs_read` (stat → read gap) — accepted as Phase 1 limitation |
| Mitigation (future) | Periodic cleanup job, temp file TTL, or `O_TMPFILE` on Linux (requires kernel support) |

### 7.3 Crash After Rename But Before Success Receipt (S-416-W-after-rename)

**This is the NEW, more serious gap** specific to `filesystem.write`:

If the process crashes **after step 4** (rename succeeds, file is visible at target) but **before step 5** (success receipt emitted):
- The file IS written and visible on the host filesystem
- NO signed receipt exists for this write
- The receipt chain has a cryptographically verifiable gap

This is NOT equivalent to TOCTOU — it directly violates the receipt chain integrity guarantee (Decision 4). It is documented as a **known limitation** specific to write operations.

### 7.4 Why This Is Acceptable for Phase 3

| Aspect | Assessment |
|--------|------------|
| Root cause | Fundamental asymmetry: host filesystem side effects cannot be retracted like guest memory |
| Comparison | `aegis_fs_read` has NO equivalent — guest memory not returned is fully controllable |
| Mitigation scope | Cannot be fixed without two-phase commit (pending receipt → confirm) or external transaction coordinator |
| Current approach | Log explicit "FAIL-CLOSED VIOLATION" audit record; return Trap; track for future mitigation |
| Future fix | Two-phase receipt emission (pending before rename, confirm after) or external KMS with atomicity |

### 7.5 Documentation

Both gaps are explicitly documented as **known limitations** in the design and will be tracked for future mitigation. The S-416-W-after-rename gap is the more severe and requires architectural work beyond Phase 3.

---

## 8. Receipt Emission Integration

### 8.1 Pattern (Exact Copy from `aegis_fs_read`)

```rust
if let Some(ref mut emitter) = caller.data_mut().receipt_emitter {
    if let Err(e) = emitter.emit(&cap_name, "write", path, size, result) {
        return Err(anyhow::anyhow!("receipt emission failed: {}", e));
    }
}
```

### 8.2 Emission Points Summary

| # | Trigger | capability | action | result | size | path |
|---|---------|------------|--------|--------|------|------|
| 1 | Happy path (AFTER successful rename) | filesystem.write | write | success | data_len | guest_path |
| 2 | Path outside root (canonical check fails) | filesystem.write | write | trap | 0 | guest_path |
| 3 | `..` segment detected (pre-canonicalize) | filesystem.write | write | trap | 0 | guest_path |
| 4 | Data size > max_bytes | filesystem.write | write | trap | data_len | guest_path |
| 5 | Guest memory OOB (path or data) | filesystem.write | write | trap | 0 | "" |
| 6 | Rename failed | filesystem.write | write | trap | data_len | guest_path |
| 7 | Emit fails AFTER successful rename (S-416-W-after-rename) | filesystem.write | write | trap | data_len | guest_path |

**Note**: Points 2, 3, 4, 5, 6 emit `result="trap"` and immediately return Trap. Point 7 is the critical asymmetry: rename succeeded (file IS written), then emit fails — we log FAIL-CLOSED VIOLATION, return Trap, but CANNOT undo the host filesystem write.

### 8.3 Fail-Closed Guarantee (REQ-510)

> If `emit()` returns `Err` at any point, the host function MUST immediately return Trap.

**For validation failures (points 2-6)**: No data is written to the target path. Temp file is cleaned up if it was created.

**For success receipt failure after rename (point 7)**: File IS already written to target path (cannot be undone). Host function returns Trap, audit log records FAIL-CLOSED VIOLATION. This is a documented known limitation — see §7.3.

This is enforced by the `return Err(...)` pattern at every emission point.

---

## 9. Testing Approach

### 9.1 WAT Module Patterns (Mirror `filesystem.read`)

```wat
;; S-1-W: Happy Write
(module
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "output.txt\00")
  (data (i32.const 2048) "hello world")
  (func (export "_start")
    (call $fs_write (i32.const 1024) (i32.const 10) (i32.const 2048) (i32.const 11))
    drop
  )
)

;; S-2-W: Outside Root (../secret.txt)
(data (i32.const 1024) "../secret.txt\00")

;; S-3-W: Path Traversal (../../etc/passwd)
(data (i32.const 1024) "../../etc/passwd\00")

;; S-5-W: Size Exceeded (data_len > max_bytes)
(data (i32.const 2048) "x" 2048)  ;; 2KB data, limit is 1KB

;; T-6-W: Guest Memory OOB (data_ptr beyond 64KB)
(data (i32.const 1024) "output.txt\00")
(call $fs_write (i32.const 1024) (i32.const 10) (i32.const 100000) (i32.const 11))

;; Symlink-W: Symlink Escape
(data (i32.const 1024) "symlink.txt\00")  ;; symlink points outside root
```

### 9.2 Test Fixtures

| Fixture | Purpose |
|---------|---------|
| `tests/fixtures/config/filesystem-write.toml` | Integration test for `Sandbox::from_config` with `filesystem.write` capability |

```toml
[[capabilities]]
name = "filesystem.write"
allowed_root = "/tmp/aegis-test-write"
max_write_bytes = 1048576
```

### 9.3 Test Functions (8 New Tests)

| Test | Scenario | Verifies |
|------|----------|----------|
| `allowed_write_succeeds` | S-1-W | File written atomically, receipt `success` |
| `denied_write_traps` | S-2-W | Trap on outside root, receipt `trap` |
| `traversal_write_traps` | S-3-W | Trap on `..`, receipt `trap` |
| `size_exceeded_write_traps` | S-5-W | Trap on size > max_bytes, receipt `trap` |
| `guest_memory_oob_write` | T-6-W | Trap on OOB data_ptr, receipt `trap` |
| `symlink_escape_write_traps` | Symlink-W | Trap on symlink escape, receipt `trap` |
| `receipt_s416_write_signing_failure` | S-416-W | Trap on signing failure (any validation point), NO target file, NO receipt added |
| `receipt_s416_write_after_rename` | S-416-W-after-rename | Trap on emit failure AFTER rename, file written, FAIL-CLOSED VIOLATION logged |

### 9.4 `force_signing_failure` Usage

```rust
// S-416-W: Force signing failure on any validation trap path
let mut emitter = sandbox.store_mut().data_mut().receipt_emitter.as_mut().unwrap();
emitter.force_signing_failure();  // Next emit() fails
let result = func.call(sandbox.store_mut(), ());
assert!(result.is_err());  // Must trap
assert_eq!(sandbox.get_receipt_chain().len(), 1);  // No new receipt

// S-416-W-after-rename: Force failure AFTER successful rename
// Requires: emit once successfully (first call), then force failure on second call
// Verify: file IS written at target path, but function returns Trap,
// and audit log contains "FAIL-CLOSED VIOLATION"
```

### 9.5 Regression Tests (Must Continue Passing)

All 71 existing tests including:
- `filesystem.read` scenarios (S-1..S-5, T-1, T-6, Symlink)
- Receipt chain tests (S-1..S-5, S-416)
- CapabilityConfig construction tests (A.1..A.3, SA-201)
- Sandbox resource limit tests (memory, table, instances, epoch)

---

## 10. Rollback Plan

From proposal — exact revert sequence:

1. **Revert `src/sandbox/mod.rs`** to pre-refactor state:
   - `CapabilityConfig.max_read_bytes` field (not `max_bytes`)
   - `From<&Capability>` using `max_read_bytes` for `FilesystemRead`
   - No `aegis_fs_write` function
   - No `FilesystemWrite` branch in `instantiate_with_capabilities()`

2. **Revert `tests/sandbox.rs`**:
   - Remove all `filesystem.write` WAT modules (`ALLOWED_WRITE`, `DENIED_WRITE`, etc.)
   - Remove all 8 new test functions
   - Remove `fs_write_capability` helper if added

3. **Delete `tests/fixtures/config/filesystem-write.toml`**

4. **Verify**: Run `cargo test` → all 71 original tests pass

---

## 11. Traceability Matrix

| Requirement | Design Section | Test Coverage |
|-------------|----------------|---------------|
| REQ-501 (host fn) | §5.1 | `allowed_write_succeeds` |
| REQ-502 (path validation) | §5.2 step 5 | `denied_write_traps`, `symlink_escape_write_traps` |
| REQ-503 (traversal) | §5.2 step 4 | `traversal_write_traps` |
| REQ-504 (size) | §5.2 step 7 | `size_exceeded_write_traps` |
| REQ-505 (atomic write) | §5.2 steps 8-9 | `allowed_write_succeeds` |
| REQ-506 (overwrite) | §5.2 step 9 | `allowed_write_succeeds` (overwrite test variant) |
| REQ-507 (register) | §5.1 | All write tests |
| REQ-508 (trap on violation) | §5.2 steps 4,5,7 | All trap tests |
| REQ-509 (7 emissions) | §8.2 | All receipt tests |
| REQ-510 (fail-closed emit) | §5.2 step 10, §6 | `receipt_s416_write_signing_failure`, `receipt_s416_write_after_rename` |
| REQ-511 (temp cleanup) | §5.2 step 9 | `receipt_s416_write_signing_failure` (validation failures clean temp) |
| REQ-512 (validate before write) | §5.2 steps 4-7 before step 8 | All validation tests |
| Unit A: REQ-104, REQ-204, REQ-464 | §4 | All 71 existing tests (regression gate) |

---

## 12. Acceptance Checklist

### Unit A (Regression Gate)
- [ ] `CapabilityConfig.max_read_bytes` → `max_bytes` renamed
- [ ] `From<&Capability>` updated for `FilesystemRead` and `FilesystemWrite`
- [ ] `aegis_fs_read` uses `cap.max_bytes`
- [ ] `cargo test` — **all 71 tests pass, zero failures**

### Unit B (Implementation)
- [ ] `aegis_fs_write` implemented with atomic write (`.aegis_tmp` + rename)
- [ ] Registered in `instantiate_with_capabilities()` for `FilesystemWrite`
- [ ] All 7 receipt emission points working (S-1-W..S-5-W, rename failure, S-416-W-after-rename)
- [ ] S-416-W fail-closed working (validation trap paths)
- [ ] S-416-W-after-rename: Trap + FAIL-CLOSED VIOLATION log after rename
- [ ] 8 new tests pass
- [ ] `tests/fixtures/config/filesystem-write.toml` created
- [ ] `cargo clippy` clean
- [ ] `cargo fmt` clean
- [ ] No regression in `filesystem.read` or receipt chain tests
# Delta: aegis-runtime-binary — Phase 9 Two-Phase Receipts

## ADDED Requirements

### REQ-820: TwoPhaseReceipts Capability

The `Capability` enum SHALL include a variant for two-phase receipt execution capability. This is **not a new host function capability** — it is a **protocol-level capability** that enables the Execute RPC to use the two-phase Prepare/Commit/Abort flow.

```rust
pub enum Capability {
    FilesystemRead(FilesystemReadParams),
    FilesystemWrite(FilesystemWriteParams),
    NetworkHttp(NetworkHttpParams),
    // NEW: Two-phase receipt protocol capability
    TwoPhaseReceipts,
}
```

**SEMANTICS**: `Capability::TwoPhaseReceipts` is a **marker capability** granted via `PolicyConfig` that authorizes the caller to use the `ExecutePrepare`, `ExecuteCommit`, and `ExecuteAbort` RPCs. When absent, only the legacy `Execute` RPC is available.

**NOTE**: This is a **sub-capability of Execute** — it does not introduce a new host function. The two-phase protocol operates at the Execute RPC boundary (D1: Execute-only scope). Host functions (`aegis_fs_read`, `aegis_fs_write`, `aegis_http_fetch`) remain legacy single-phase and do not require or use this capability.

### REQ-821: PolicyConfig TwoPhaseReceipts Parsing

`PolicyConfig::try_into_capabilities()` SHALL recognize a `two_phase_receipts = true` (or `[capabilities.two_phase_receipts]`) key in the TOML config and produce `Capability::TwoPhaseReceipts` in the capabilities vector. When absent, the capability is not granted (default deny).

### REQ-822: ExecutePrepare Requires TwoPhaseReceipts Capability

The `ExecutePrepare` RPC handler SHALL validate that `Capability::TwoPhaseReceipts` is present in the granted capabilities. If not granted, return `FAILED_PRECONDITION` with error "two-phase receipts capability not granted". The legacy `Execute` RPC does NOT require this capability (backward compat).

### REQ-823: ExecuteCommit/Abort Implicitly Authorized by Prepare

`ExecuteCommit` and `ExecuteAbort` RPCs SHALL NOT re-validate capabilities — they are authorized by virtue of the `prepare_hash` which was issued by a successful `ExecutePrepare` that already validated the capability. The `prepare_hash` lookup in the pending map is the authorization check.

---

## ADDED Scenarios

| # | Scenario | Given | When | Then |
|---|----------|-------|------|------|
| S-970 | TwoPhaseReceipts granted | Config with `two_phase_receipts = true` | ExecutePrepare called | `success=true`, prepare receipt emitted |
| S-971 | TwoPhaseReceipts denied | Config WITHOUT `two_phase_receipts` | ExecutePrepare called | `success=false`, "two-phase receipts capability not granted" |
| S-972 | Legacy Execute works without capability | Config WITHOUT `two_phase_receipts` | Legacy Execute called | Works (backward compat) |
| S-973 | ExecuteCommit authorized by prepare_hash | Prepare succeeded, commit called | ExecuteCommit with prepare_hash | `success=true`, no capability re-check |
| S-974 | ExecuteAbort authorized by prepare_hash | Prepare succeeded, abort called | ExecuteAbort with prepare_hash | `success=true`, no capability re-check |

---

## REMOVED Requirements

None.

## RENAMED Requirements

None.

---

## Traceability

| Requirement | Scenario(s) |
|-------------|-------------|
| REQ-820 | S-970, S-971 |
| REQ-821 | S-970, S-971 |
| REQ-822 | S-970, S-971 |
| REQ-823 | S-973, S-974 |
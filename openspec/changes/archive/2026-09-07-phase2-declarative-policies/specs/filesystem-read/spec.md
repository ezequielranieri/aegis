# Delta Spec: filesystem-read — Phase 2 Changes

## ADDED Requirements

### REQ-201: Typed Capability Enum
System SHALL define `Capability` as a Rust enum with variants `FilesystemRead(FilesystemReadParams)`, `FilesystemWrite(FilesystemWriteParams)`, `NetworkHttp(NetworkHttpParams)`. Each variant carries typed params structs. Derives `Debug, Clone, Serialize, Deserialize`.

### REQ-202: Typed Params Structs
System SHALL define `FilesystemReadParams { allowed_root: PathBuf, max_read_bytes: u64 }`, `FilesystemWriteParams { allowed_root: PathBuf, max_write_bytes: u64 }`, `NetworkHttpParams { allowed_hosts: Vec<String>, max_requests_per_second: u64 }`.

### REQ-203: capability_name() Method
System SHALL provide `capability_name(&self) -> &'static str` method on `Capability` returning the variant name (e.g., `"filesystem.read"`).

### REQ-204: CapabilityConfig::from Enum Match
System SHALL update `CapabilityConfig::from(&Capability)` to `match` on enum variants instead of parsing `HashMap<String, Value>`. Each arm constructs `CapabilityConfig` with correct fields.

### REQ-205: instantiate_with_capabilities Enum Iteration
System SHALL update `Sandbox::instantiate_with_capabilities(&[Capability])` to iterate enum variants and register host functions per variant using `capability_name()`.

## MODIFIED Requirements

### REQ-106: Capability-Driven Registration (Updated)
**Previous**: `Sandbox::instantiate(wasm_bytes, capabilities)` SHALL iterate capabilities. For each `filesystem.read`, register `aegis::fs_read` with `allowed_root` and `max_read_bytes` captured in the closure. Capability is `{ name: String, params: HashMap<String, Value> }`.

**Updated**: `Sandbox::instantiate_with_capabilities(wasm_bytes, capabilities)` SHALL iterate `&[Capability]` enum. For each `Capability::FilesystemRead(params)`, register `aegis::fs_read` using `capability_name()` and params fields. Capability is now a typed enum.

### Public API — Capability Type (Updated)
**Previous**: `Capability { name: String, params: HashMap<String, Value> }`
**Updated**: `Capability` is a typed enum with `FilesystemRead(FilesystemReadParams)`, `FilesystemWrite(FilesystemWriteParams)`, `NetworkHttp(NetworkHttpParams)` variants.

## REMOVED Requirements

### REQ-108: Receipt Stub (Removed from filesystem-read domain)
**Reason**: Receipt stubs are out of scope for Phase 2; moved to Phase 3+ as part of signed execution receipts.
**Migration**: Receipt functionality will be re-implemented in Phase 3 with `emit_capability_event` calling `capability.capability_name()`.

## RENAMED Requirements
None.

## Delta Metadata
- **Source**: Phase 2 — Declarative Policies change
- **Domain**: filesystem-read
- **Baseline**: Phase 1 spec at `openspec/specs/filesystem-read/spec.md`
- **Changes**: Capability struct → typed enum; adds REQ-201..REQ-206; modifies REQ-106 and Public API; removes REQ-108

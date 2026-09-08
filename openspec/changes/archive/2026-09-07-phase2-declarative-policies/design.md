# Design: Phase 2 — Declarative Policies

## Technical Approach

Refactor the generic `Capability { name: String, params: HashMap }` to a typed enum `Capability::FilesystemRead(FilesystemReadParams)` etc. to enable `serde(tag = "name")` deserialization from TOML `[[capabilities]]` arrays directly into `Vec<Capability>`. Add `src/config/` module for fail-closed config loading with `ConfigError` variants. Two sequential units with regression gate: Unit A (enum refactor, 18 Phase 1 tests must pass), then Unit B (TOML parsing + `Sandbox::from_config`).

## Architecture Decisions

### Decision: Capability as Typed Enum vs Generic Struct

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Keep generic `Capability { name, params }` + manual TOML→Vec conversion | No enum, but runtime string matching, no compile-time exhaustiveness | ❌ Rejected |
| **Typed enum `Capability { FilesystemRead, FilesystemWrite, NetworkHttp }`** | `serde(tag="name")` maps TOML directly; `capability_name()` is compile-time; exhaustive match in `CapabilityConfig::from` | ✅ **Chosen** |

### Decision: `serde(tag = "name")` for TOML Deserialization

| Option | Tradeoff | Decision |
|--------|----------|----------|
| Manual parsing: `Vec<CapabilityDef>` → manual `match` → `Capability` | Explicit, but boilerplate for each capability | ❌ Rejected |
| **`#[serde(tag = "name")]` on enum** | Zero boilerplate; TOML `name` field maps to enum variant; unknown names fail at deserialize time | ✅ **Chosen** |

### Decision: ConfigError Variants and Validation Sequence

| Variant | When Triggered | Fail-Closed? |
|---------|----------------|--------------|
| `NotFound(path)` | Config file doesn't exist at either path | Yes |
| `ParseError(toml::de::Error)` | TOML syntax invalid | Yes |
| `UnknownCapability { name, allowed }` | `name` not in builtin enum variants | Yes |
| `MissingField { capability, field }` | Required field absent for variant | Yes |
| `DuplicateCapability { name }` | Two `[[capabilities]]` with same `name` | Yes |
| `InvalidPath { capability, path }` | `allowed_root` not absolute or canonicalize fails | Yes |
| `Io(std::io::Error)` | File read error | Yes |

**Validation sequence (fail fast):**
1. File read → `NotFound` / `Io`
2. TOML parse → `ParseError`
3. Deserialize `PolicyConfig` → `UnknownCapability` (via `serde` tag mismatch)
4. Post-deserialize validation pass: check required fields per variant → `MissingField`
5. Check duplicates in `Vec<CapabilityDef>` → `DuplicateCapability`
6. Canonicalize `allowed_root` → `InvalidPath`

### Decision: Path Canonicalization Location

| Location | Tradeoff | Decision |
|----------|----------|----------|
| In `FilesystemReadParams::try_from` | Early, but `PathBuf` in struct not yet canonicalized | ❌ Rejected |
| In `CapabilityConfig::from` | **Current Phase 1 behavior** — `std::fs::canonicalize` with fallback | ✅ **Chosen** (consistency) |
| In `Sandbox::from_config` | Late, would require pre-validation | ❌ Rejected |

**Rationale:** Phase 1 `CapabilityConfig::from` already does `std::fs::canonicalize(&allowed_root).unwrap_or(allowed_root)`. Keep this behavior — config loading validates `allowed_root` is non-empty absolute path, then `CapabilityConfig::from` canonicalizes.

### Decision: Regression Gate Process

| Aspect | Decision |
|--------|----------|
| **Gate** | Unit A complete only when `cargo test` shows 18/18 Phase 1 tests pass |
| **Automation** | Not CI-enforced in Phase 2; documented as mandatory manual step before Unit B |
| **Rollback** | If gate fails, revert Unit A changes per rollback plan in proposal |

## Data Flow

```
TOML Config File
       │
       ▼
PolicyConfig::load(path) ──→ ConfigError::NotFound/ParseError
       │
       ▼
toml::from_str → PolicyConfig { capabilities: Vec<CapabilityDef> }
       │
       ▼
CapabilityDef (serde tag="name") ──→ Capability enum (FilesystemRead/Write/NetworkHttp)
       │                                        │
       │                                        ▼
       │                              UnknownCapability if tag mismatch
       ▼
Validation pass:
  - Required fields per variant → MissingField
  - Duplicate names → DuplicateCapability
  - allowed_root absolute + canonicalizable → InvalidPath
       │
       ▼
Vec<Capability> ──→ Sandbox::from_config
       │
       ▼
Sandbox::new_with_limits() → Sandbox
       │
       ▼
instantiate_with_capabilities(wasm, capabilities)
       │
       ▼
CapabilityConfig::from(&Capability) [match on enum]
       │
       ▼
Vec<CapabilityConfig> stored in SandboxState
       │
       ▼
Linker registers host functions per variant
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `Cargo.toml` | Modify | Add `toml = "0.8"` dependency |
| `src/capabilities/mod.rs` | Modify | Replace `struct Capability` with `enum Capability`; add `FilesystemReadParams`, `FilesystemWriteParams`, `NetworkHttpParams`; add `capability_name()` method |
| `src/sandbox/mod.rs` | Modify | Update `CapabilityConfig::from` to `match` on enum; add `Sandbox::from_config` constructor |
| `src/config/mod.rs` | Create | New module: `PolicyConfig`, `CapabilityDef`, `ConfigError`, `PolicyConfig::load()`, validation logic |
| `src/lib.rs` | Modify | Re-export `config` module and `ConfigError` |
| `tests/config_parsing.rs` | Create | Unit tests for TOML parsing, validation, error variants |
| `tests/sandbox_config.rs` | Create | Integration tests for `Sandbox::from_config` with valid/invalid configs |
| `tests/fixtures/config/` | Create | Test fixtures: `valid.toml`, `multi_cap.toml`, `missing_field.toml`, `unknown_cap.toml`, `duplicate_cap.toml`, `invalid_path.toml` |

## Interfaces / Contracts

### Capability Enum (src/capabilities/mod.rs)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "name")]
pub enum Capability {
    #[serde(rename = "filesystem.read")]
    FilesystemRead(FilesystemReadParams),
    #[serde(rename = "filesystem.write")]
    FilesystemWrite(FilesystemWriteParams),
    #[serde(rename = "network.http")]
    NetworkHttp(NetworkHttpParams),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemReadParams {
    pub allowed_root: PathBuf,
    pub max_read_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemWriteParams {
    pub allowed_root: PathBuf,
    pub max_write_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkHttpParams {
    pub allowed_hosts: Vec<String>,
    pub max_requests_per_second: u64,
}

impl Capability {
    pub fn capability_name(&self) -> &'static str {
        match self {
            Capability::FilesystemRead(_) => "filesystem.read",
            Capability::FilesystemWrite(_) => "filesystem.write",
            Capability::NetworkHttp(_) => "network.http",
        }
    }
}
```

### Config Types (src/config/mod.rs)

```rust
#[derive(Debug, Deserialize)]
pub struct PolicyConfig {
    pub capabilities: Vec<CapabilityDef>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "name")]
pub enum CapabilityDef {
    #[serde(rename = "filesystem.read")]
    FilesystemRead { allowed_root: String, max_read_bytes: u64 },
    #[serde(rename = "filesystem.write")]
    FilesystemWrite { allowed_root: String, max_write_bytes: u64 },
    #[serde(rename = "network.http")]
    NetworkHttp { allowed_hosts: Vec<String>, max_requests_per_second: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Config file not found: {0}")]
    NotFound(String),
    #[error("Failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),
    #[error("Missing required field '{field}' for capability '{capability}'")]
    MissingField { capability: String, field: String },
    #[error("Unknown capability type '{name}'. Must be one of: {allowed:?}")]
    UnknownCapability { name: String, allowed: Vec<String> },
    #[error("Duplicate capability '{name}' in config")]
    DuplicateCapability { name: String },
    #[error("Invalid path '{path}' for capability '{capability}'")]
    InvalidPath { capability: String, path: String },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl PolicyConfig {
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        // 1. Try project config, then fallback
        let paths = [path, &default_config_path()];
        let content = paths.iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .ok_or_else(|| ConfigError::NotFound(path.to_string()))?;

        // 2. Parse TOML
        let config: PolicyConfig = toml::from_str(&content)?;

        // 3. Validate
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        // Check duplicates
        let mut seen = std::collections::HashSet::new();
        for cap in &self.capabilities {
            let name = cap.name();
            if !seen.insert(name) {
                return Err(ConfigError::DuplicateCapability { name: name.to_string() });
            }
        }
        // Required fields validated in try_into_capabilities
        Ok(())
    }

    pub fn try_into_capabilities(self) -> Result<Vec<Capability>, ConfigError> {
        self.capabilities.into_iter().map(|c| c.try_into()).collect()
    }
}

impl CapabilityDef {
    fn name(&self) -> &'static str {
        match self {
            CapabilityDef::FilesystemRead { .. } => "filesystem.read",
            CapabilityDef::FilesystemWrite { .. } => "filesystem.write",
            CapabilityDef::NetworkHttp { .. } => "network.http",
        }
    }

    fn try_into(self) -> Result<Capability, ConfigError> {
        match self {
            CapabilityDef::FilesystemRead { allowed_root, max_read_bytes } => {
                if max_read_bytes == 0 {
                    return Err(ConfigError::MissingField {
                        capability: "filesystem.read".into(),
                        field: "max_read_bytes (> 0)".into(),
                    });
                }
                let root = PathBuf::from(&allowed_root);
                if !root.is_absolute() {
                    return Err(ConfigError::InvalidPath {
                        capability: "filesystem.read".into(),
                        path: allowed_root,
                    });
                }
                // Verify canonicalizable
                std::fs::canonicalize(&root).map_err(|_| ConfigError::InvalidPath {
                    capability: "filesystem.read".into(),
                    path: allowed_root,
                })?;
                Ok(Capability::FilesystemRead(FilesystemReadParams {
                    allowed_root: root,
                    max_read_bytes,
                }))
            }
            CapabilityDef::FilesystemWrite { .. } => todo!("Phase 3"),
            CapabilityDef::NetworkHttp { .. } => todo!("Phase 3"),
        }
    }
}
```

### Sandbox::from_config (src/sandbox/mod.rs)

```rust
impl Sandbox {
    pub fn from_config(config_path: &str) -> Result<Self, ConfigError> {
        let policy_config = crate::config::PolicyConfig::load(config_path)?;
        let capabilities = policy_config.try_into_capabilities()?;

        // Create sandbox with default limits
        let mut sandbox = Self::new_with_limits(SandboxConfig::default())?;

        // Convert to CapabilityConfig and store
        let capability_configs: Vec<CapabilityConfig> =
            capabilities.iter().map(CapabilityConfig::from).collect();
        sandbox.store.data_mut().capabilities = capability_configs;

        Ok(sandbox)
    }
}
```

### Updated CapabilityConfig::from (src/sandbox/mod.rs)

```rust
impl From<&Capability> for CapabilityConfig {
    fn from(cap: &Capability) -> Self {
        match cap {
            Capability::FilesystemRead(params) => Self {
                name: "filesystem.read".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_read_bytes: params.max_read_bytes,
            },
            Capability::FilesystemWrite(params) => Self {
                name: "filesystem.write".to_string(),
                allowed_root: std::fs::canonicalize(&params.allowed_root)
                    .unwrap_or_else(|_| params.allowed_root.clone()),
                max_read_bytes: params.max_write_bytes, // reuse field
            },
            Capability::NetworkHttp(params) => Self {
                name: "network.http".to_string(),
                allowed_root: PathBuf::new(), // unused for network
                max_read_bytes: params.max_requests_per_second, // repurpose
            },
        }
    }
}
```

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Unit | `PolicyConfig::load` parses valid TOML | `toml::from_str` on fixture files, assert `Vec<Capability>` |
| Unit | `serde(tag="name")` rejects unknown capability | TOML with `name = "unknown"` → `ConfigError::UnknownCapability` |
| Unit | Missing required field → `MissingField` | TOML without `allowed_root` → error |
| Unit | `max_read_bytes = 0` → `MissingField` | TOML with zero value → validation error |
| Unit | Relative `allowed_root` → `InvalidPath` | TOML with relative path → error |
| Unit | Duplicate capability names → `DuplicateCapability` | Two `[[capabilities]]` with same `name` → error |
| Unit | Config not found → `NotFound` | Path to nonexistent file → error |
| Unit | Fallback path resolution | No `.aegis/config.toml`, `~/.config/aegis/config.toml` exists → loads fallback |
| Integration | `Sandbox::from_config` with valid config | Creates sandbox, capability registered, host function works |
| Integration | `Sandbox::from_config` with invalid config | Returns `ConfigError`, sandbox never created |
| Integration | Multi-capability config | `filesystem.read` + `network.http` (Phase 3 stub) both parsed |
| Integration | Regression gate | `cargo test` — all 18 Phase 1 tests pass after Unit A |

## Threat Matrix

**N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary.** This change is purely configuration parsing and type refactoring within the existing sandbox process.

## Migration / Rollout

No migration required. Phase 1 programmatic API (`Sandbox::instantiate_with_capabilities`) remains functional. New `Sandbox::from_config` is additive.

## Open Questions

- [ ] Should `CapabilityDef` use a `params: HashMap` fallback for future extensibility vs typed fields? Current design uses typed fields per variant — aligns with `serde(tag="name")` and compile-time safety.
- [ ] Should `Sandbox::from_config` accept `Path` or `&str`? Current: `&str` for simplicity, can generalize later.
- [ ] Phase 3: How to handle `network.http` validation when `PolicyEngine` integration happens? Deferred.
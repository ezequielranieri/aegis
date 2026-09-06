# Testing Capabilities — aegis

**Strict TDD Mode**: disabled
**Detected**: 2026-09-06

## Projects

| Relative path | Stack | Test command | Framework |
| ------------- | ----- | ------------ | --------- |
| `.` | Rust (Wasmtime, Tokio, OpenTelemetry) | `cargo test` | cargo test / built-in |

## Test Layers

| Relative path | Layer       | Available | Tool        |
| ------------- | ----------- | --------- | ----------- |
| `.` | Unit        | ✅        | cargo test  |
| `.` | Integration | ❌        | —           |
| `.` | E2E         | ❌        | —           |

## Coverage

| Relative path | Available | Command |
| ------------- | --------- | ------- |
| `.` | ✅ | `cargo tarpaulin --out Html` |

## Quality Tools

| Relative path | Tool         | Available | Command        |
| ------------- | ------------ | --------- | -------------- |
| `.` | Linter       | ✅        | `cargo clippy -- -D warnings` |
| `.` | Type checker | ✅        | `cargo check` |
| `.` | Formatter    | ✅        | `cargo fmt -- --check` |

## Notes

- This is a new Rust project with Wasmtime-based sandboxing
- No explicit workspace-level test command exists yet (single project)
- Strict TDD defaults to false for new projects without workspace-wide test command
- Testing capabilities will be updated as the project evolves
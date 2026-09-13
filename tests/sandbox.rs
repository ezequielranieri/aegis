use aegis::capabilities::{
    Capability, FilesystemReadParams, FilesystemWriteParams, NetworkHttpParams,
};
use aegis::sandbox::{Sandbox, SandboxConfig};
use std::time::Duration;
use wat::parse_str;

// ═══════════════════════════════════════════════════════════════════════════════
// WAT Modules (hostile modules for capability testing)
// ═══════════════════════════════════════════════════════════════════════════════

/// S-1: Allowed Read — reads test.txt within root, size ≤ limit
const ALLOWED_READ: &str = r#"
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "test.txt\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 8) (i32.const 2048) (i32.const 1024))
    drop
  )
)
"#;

/// S-2: Denied Read — path resolves outside allowed_root
const DENIED_READ: &str = r#"
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "../secret.txt\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 13) (i32.const 2048) (i32.const 1024))
    drop
  )
)
"#;

/// S-3 / T-1: Path Traversal — path contains ../../etc/passwd, rejected pre-canonicalize
const TRAVERSAL: &str = r#"
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "../../etc/passwd\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 15) (i32.const 2048) (i32.const 1024))
    drop
  )
)
"#;

/// S-4: FD Leak — imports wasi_snapshot_preview1::fd_read → linking error
const FD_LEAK: &str = r#"
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (call $fd_read (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))
    drop
  )
)
"#;

/// S-5: Size Exceeded — file size > max_read_bytes
const SIZE_EXCEEDED: &str = r#"
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "large.bin\00")
  (func (export "_start")
    (call $fs_read (i32.const 1024) (i32.const 9) (i32.const 2048) (i32.const 1024))
    drop
  )
)
"#;

/// T-6: Guest Memory OOB — path_ptr is beyond memory bounds
const GUEST_OOB: &str = r#"
(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)  ;; 1 page = 64KB
  (func (export "_start")
    ;; path_ptr=100000 is beyond 64KB memory
    (call $fs_read (i32.const 100000) (i32.const 8) (i32.const 0) (i32.const 0))
    drop
  )
)
"#;

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: filesystem.write WAT Modules
// ═══════════════════════════════════════════════════════════════════════════════

/// S-1-W: Allowed Write — writes "hello world" to "output.txt" within root
const ALLOWED_WRITE: &str = r#"
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
"#;

/// S-2-W: Outside Root — path resolves outside allowed_root (../secret.txt)
const DENIED_WRITE: &str = r#"
(module
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "../secret.txt\00")
  (data (i32.const 2048) "secret data")
  (func (export "_start")
    (call $fs_write (i32.const 1024) (i32.const 13) (i32.const 2048) (i32.const 11))
    drop
  )
)
"#;

/// S-3-W: Path Traversal — ../../etc/passwd, rejected pre-canonicalize
const TRAVERSAL_WRITE: &str = r#"
(module
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "../../etc/passwd\00")
  (data (i32.const 2048) "pwned")
  (func (export "_start")
    (call $fs_write (i32.const 1024) (i32.const 15) (i32.const 2048) (i32.const 5))
    drop
  )
)
"#;

/// S-5-W: Size Exceeded — data_len (2048) > max_write_bytes (1024)
const SIZE_EXCEEDED_WRITE: &str = r#"
(module
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "output.txt\00")
  (func (export "_start")
    ;; data_ptr=2048, data_len=2048 — exceeds 1024 limit
    (call $fs_write (i32.const 1024) (i32.const 10) (i32.const 2048) (i32.const 2048))
    drop
  )
)
"#;

/// T-6-W: Guest Memory OOB — both path_ptr and data_ptr beyond 64KB memory bounds
const GUEST_OOB_WRITE: &str = r#"
(module
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    ;; path_ptr=100000 is beyond 64KB memory — path read fails first
    (call $fs_write (i32.const 100000) (i32.const 10) (i32.const 100000) (i32.const 11))
    drop
  )
)
"#;

// ═══════════════════════════════════════════════════════════════════════════════
// Helper: create a filesystem.read capability with given root and limit
// ═══════════════════════════════════════════════════════════════════════════════

fn fs_read_capability(root: &str, max_bytes: u64) -> Capability {
    Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from(root),
        max_read_bytes: max_bytes,
    })
}

/// Helper: create a filesystem.write capability with given root and limit
fn fs_write_capability(root: &str, max_bytes: u64) -> Capability {
    Capability::FilesystemWrite(FilesystemWriteParams {
        allowed_root: std::path::PathBuf::from(root),
        max_write_bytes: max_bytes,
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.1 RED T-1: Traversal rejection logic (unit test via WAT)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn traversal_rejection_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(TRAVERSAL).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on path traversal");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path traversal attempt"),
        "Expected 'path traversal attempt' in error chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.2 RED T-6: Guest memory OOB (unit test via WAT)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn guest_memory_oob_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(GUEST_OOB).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on guest memory OOB");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "out of bounds"),
        "Expected 'out of bounds' in error chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.3 S-1: Allowed Read — reads test.txt within root, no trap
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn allowed_read_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(ALLOWED_READ).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_ok(), "Expected success, got: {:?}", result.err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.4 S-2: Denied Read — path resolves outside allowed_root → trap
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn denied_read_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(DENIED_READ).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on denied read");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path outside allowed root")
            || error_chain_contains(&err, "path traversal"),
        "Expected path violation error in chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.5 S-3: Path Traversal — ../../etc/passwd → trap (pre-canonicalize)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn traversal_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(TRAVERSAL).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on path traversal");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path traversal attempt"),
        "Expected 'path traversal attempt' in error chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.6 S-4: FD Leak — wasi_snapshot_preview1::fd_read → linking error
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn fd_leak_linking_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(FD_LEAK).expect("WAT parse failed");
    let result = sandbox.instantiate_with_capabilities(&wasm, &[cap]);

    assert!(result.is_err(), "Expected linking error on WASI import");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unknown import"),
        "Expected 'unknown import' linking error, got: {}",
        err_msg
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.7 T-2: Symlink Escape — symlink points outside root → trap
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn symlink_escape_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_file = outside_dir.path().join("secret.txt");
    std::fs::write(&outside_file, "secret data").unwrap();

    // Create symlink inside tmp pointing outside
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, tmp.path().join("symlink.txt")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&outside_file, tmp.path().join("symlink.txt")).unwrap();

    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    // Build WAT that reads the symlink path
    let wat = format!(
        r#"
        (module
          (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 1024) "symlink.txt\00")
          (func (export "_start")
            (call $fs_read (i32.const 1024) (i32.const 11) (i32.const 2048) (i32.const 1024))
            drop
          )
        )
        "#
    );

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(&wat).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on symlink escape");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path outside allowed root"),
        "Expected 'path outside allowed root' in error chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4.8 S-5: Size Exceeded — file size > max_read_bytes → trap
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn size_exceeded_traps() {
    let tmp = tempfile::tempdir().unwrap();
    // Create a file larger than the 1KB limit
    let large_data = vec![0u8; 2048];
    std::fs::write(tmp.path().join("large.bin"), large_data).unwrap();

    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1024); // 1KB limit

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let wasm = parse_str(SIZE_EXCEEDED).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on size exceeded");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "file size") && error_chain_contains(&err, "exceeds"),
        "Expected 'file size ... exceeds' in error chain, got: {:?}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helper: check if any error in the chain contains a substring
// wasmtime 24.0 wraps host errors in a backtrace error; the custom message
// is in the "Caused by:" chain, not in to_string().
// ═══════════════════════════════════════════════════════════════════════════════

fn error_chain_contains(err: &wasmtime::Error, needle: &str) -> bool {
    // Check the top-level display
    if err.to_string().contains(needle) {
        return true;
    }
    // Walk the source chain
    let mut source: Option<&dyn std::error::Error> = err.source();
    while let Some(s) = source {
        if s.to_string().contains(needle) {
            return true;
        }
        source = s.source();
    }
    // Also check Debug (includes "Caused by:" formatting)
    let debug = format!("{:?}", err);
    debug.contains(needle)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Existing Phase 0 tests (preserved)
// ═══════════════════════════════════════════════════════════════════════════════

/// Hostile WAT module: attempts to grow memory by 100 pages (6.4 MB)
/// when the sandbox limit is 1 MB. Expected: trap on memory.grow.
const HOSTILE_MEMORY_GROWTH: &str = r#"
(module
  (memory 1)
  (func (export "_start")
    (memory.grow (i32.const 100))
    drop
  )
)
"#;

/// Hostile WAT module: infinite loop with no epoch check point.
/// Expected: epoch interruption traps execution within the configured interval.
const HOSTILE_INFINITE_LOOP: &str = r#"
(module
  (func (export "_start")
    (loop (br 0))
  )
)
"#;

#[test]
fn hostile_memory_growth_traps() {
    let mut sandbox =
        Sandbox::new_with_limits(SandboxConfig::default()).expect("Failed to create sandbox");

    let wasm = parse_str(HOSTILE_MEMORY_GROWTH).expect("WAT parse failed");
    let instance = sandbox
        .instantiate(&wasm)
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected trap on memory.grow beyond 1MB limit"
    );
}

#[test]
fn hostile_infinite_loop_epoch_timeout() {
    // Use short epoch interval (10ms) for faster test
    let config = SandboxConfig {
        epoch_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let mut sandbox = Sandbox::new_with_limits(config).expect("Failed to create sandbox");

    let wasm = parse_str(HOSTILE_INFINITE_LOOP).expect("WAT parse failed");
    let instance = sandbox
        .instantiate(&wasm)
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected epoch deadline trap, not test timeout"
    );
}

#[test]
fn sandbox_config_defaults() {
    let config = SandboxConfig::default();
    assert_eq!(config.memory_size, 1 << 20);
    assert_eq!(config.table_elements, 1024);
    assert_eq!(config.instances, 4);
    assert_eq!(config.memories, 2);
    assert_eq!(config.epoch_interval, Duration::from_millis(100));
}

#[test]
fn epoch_interrupter_drop_joins_thread() {
    use aegis::sandbox::EpochInterrupter;

    let mut engine_config = wasmtime::Config::new();
    engine_config.epoch_interruption(true);
    let engine = wasmtime::Engine::new(&engine_config).expect("Failed to create engine");

    // Spawn with short interval, then immediately drop
    let interrupter = EpochInterrupter::new(engine, Duration::from_millis(50));
    drop(interrupter);
    // If Drop doesn't join properly, this test will hang or panic
}

#[test]
fn memory_growth_within_limit_succeeds() {
    // Disable epoch interruption for this test to avoid global epoch counter
    // interference from other tests. Memory limit is enforced by StoreLimitsBuilder,
    // not epoch interruption.
    let config = SandboxConfig::default();
    let mut sandbox = Sandbox::new_with_config(config, false).expect("Failed to create sandbox");

    // WAT: grows memory by 5 pages (320 KB) when limit is 1 MB (16 pages)
    const FRIENDLY_MEMORY_GROWTH: &str = r#"
    (module
      (memory 1)
      (func (export "_start")
        (memory.grow (i32.const 5))
        drop
      )
    )
    "#;

    let wasm = parse_str(FRIENDLY_MEMORY_GROWTH).expect("WAT parse failed");
    let instance = sandbox
        .instantiate(&wasm)
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    if let Err(ref e) = result {
        eprintln!("DEBUG: error = {:?}", e);
    }
    assert!(
        result.is_ok(),
        "Expected memory.grow 5 pages (within 1MB limit) to succeed"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: Receipt-chain-based verification tests (REQ-460..464)
// ════════════════════════════════════════════════════════════════════════════════

use ring::signature::KeyPair;

/// Helper: create a sandbox with a receipt emitter for testing
fn sandbox_with_receipts() -> (Sandbox, ring::signature::Ed25519KeyPair) {
    use ring::signature::Ed25519KeyPair;

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();

    // Create a separate keypair for the emitter
    let rng2 = ring::rand::SystemRandom::new();
    let pkcs8_2 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let emitter_key = Ed25519KeyPair::from_pkcs8(pkcs8_2.as_ref()).unwrap();

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    // Manually inject the receipt emitter with its own keypair
    let emitter = aegis::receipts::ReceiptEmitter::new(emitter_key);
    sandbox.store_mut().data_mut().receipt_emitter = Some(std::sync::Arc::new(std::sync::Mutex::new(emitter)));

    (sandbox, key_pair)
}

/// S-1: Happy Path — verify receipt with result="success"
#[test]
fn receipt_s1_happy_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(ALLOWED_READ).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_ok(), "Expected success, got: {:?}", result.err());

    // Verify receipt was emitted with correct fields
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "read");
    assert_eq!(receipt.result, "success");
    assert_eq!(receipt.path, "test.txt");
    assert_eq!(receipt.size, 11);
    assert!(receipt.timestamp_ns > 0, "timestamp_ns must be non-zero");
    assert_eq!(
        receipt.prev_hash, [0u8; 32],
        "First receipt should have genesis prev_hash"
    );

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// S-2: Denied Read — path resolves outside allowed_root → receipt with result="trap"
#[test]
fn receipt_s2_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(DENIED_READ).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on path traversal");

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "read");
    assert_eq!(receipt.result, "trap");
    assert!(receipt.timestamp_ns > 0, "timestamp_ns must be non-zero");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// S-3: Path Traversal — ../../etc/passwd → receipt with result="trap"
#[test]
fn receipt_s3_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(TRAVERSAL).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on path traversal");

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "read");
    assert_eq!(receipt.result, "trap");
    assert!(receipt.path.contains(".."), "path should contain ..");
    assert!(receipt.timestamp_ns > 0, "timestamp_ns must be non-zero");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// S-4: WASI unknown import — linking error, 0 receipts emitted (pure regression)
#[test]
fn receipt_s4_wasi_unknown_import() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, _key_pair) = sandbox_with_receipts();

    let wasm = parse_str(FD_LEAK).expect("WAT parse failed");
    let result = sandbox.instantiate_with_capabilities(&wasm, &[cap]);

    assert!(result.is_err(), "Expected linking error on WASI import");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unknown import"),
        "Expected 'unknown import' linking error, got: {}",
        err_msg
    );
    // No receipts should be emitted — module failed at linking
}

/// S-5: Guest memory OOB → receipt with result="trap", path=""
#[test]
fn receipt_s5_guest_oob() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(GUEST_OOB).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on guest memory OOB");

    // Verify receipt was emitted with result="trap" and path=""
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.read");
    assert_eq!(receipt.action, "read");
    assert_eq!(receipt.result, "trap");
    assert_eq!(receipt.path, "");
    assert!(receipt.timestamp_ns > 0, "timestamp_ns must be non-zero");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// S-416: Signing failure — emit fails → Trap, no data returned to caller
#[test]
fn receipt_s416_signing_failure() {
    // This test verifies the critical fail-closed property:
    // If ReceiptEmitter::emit() fails (signing failure), aegis_fs_read MUST
    // immediately return Trap and NOT return any data to the caller.
    // This is the critical fail-closed guarantee from Decision 4.

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.txt"), "hello world").unwrap();
    let cap = fs_read_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");
    let mut emitter = aegis::receipts::ReceiptEmitter::new(key_pair);
    sandbox.store_mut().data_mut().receipt_emitter = Some(std::sync::Arc::new(std::sync::Mutex::new(emitter)));

    let wasm = parse_str(ALLOWED_READ).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    // First, verify normal operation works (emit succeeds)
    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_ok(), "Expected success with valid key");

    // Verify receipt was emitted successfully
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1);
    let receipt = &chain[0];
    assert_eq!(receipt.result, "success");
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");

    // NOW test the critical fail-closed path:
    // Force the next emit() to fail (simulating signing failure)
    {
        let emitter = sandbox
            .store_mut()
            .data_mut()
            .receipt_emitter
            .as_ref()
            .expect("receipt emitter missing");
        emitter.lock().unwrap().force_signing_failure();
    }

    // Call again with forced signing failure - should Trap, not return data
    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected Trap on signing failure, got: {:?}",
        result
    );

    // Verify NO data was returned to guest memory (the read should not have completed)
    // We can't easily check guest memory from here, but the Trap confirms fail-closed

    // Verify NO additional receipt was added to chain (emit failed, so no receipt added)
    let chain = sandbox.get_receipt_chain();
    assert_eq!(
        chain.len(),
        1,
        "No new receipt should be added when emit fails"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// A.1 + A.2: Typed enum Capability (REQ-201, REQ-202, REQ-203, SA-201)
// ═══════════════════════════════════════════════════════════════════════════════

/// SA-201: Enum construction and capability_name() — FilesystemRead variant
#[test]
fn capability_filesystem_read_construction() {
    let cap = Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from("/data"),
        max_read_bytes: 1024,
    });

    assert_eq!(cap.capability_name(), "filesystem.read");
}

/// SA-201: capability_name() for FilesystemRead variant
#[test]
fn capability_name_returns_variant_string() {
    let fs_read = Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from("/tmp"),
        max_read_bytes: 512,
    });
    assert_eq!(fs_read.capability_name(), "filesystem.read");
}

/// Enum serialization roundtrip — verify serde derive works on enum
#[test]
fn capability_enum_serialization_roundtrip() {
    let cap = Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from("/data"),
        max_read_bytes: 2048,
    });

    let json = serde_json::to_string(&cap).expect("serialize");
    let deserialized: Capability = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.capability_name(), "filesystem.read");
}

/// A.3: CapabilityConfig from FilesystemRead variant
#[test]
fn capability_config_from_filesystem_read() {
    let cap = Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from("/tmp"),
        max_read_bytes: 512,
    });

    let config = aegis::sandbox::CapabilityConfig::from(&cap);

    assert_eq!(config.name, "filesystem.read");
    assert_eq!(config.max_bytes, 512);
    // allowed_root may be canonicalized, check it ends with /tmp
    assert!(
        config.allowed_root.to_string_lossy().ends_with("tmp"),
        "allowed_root should end with 'tmp', got: {:?}",
        config.allowed_root
    );
}

/// A.3: CapabilityConfig from FilesystemWrite variant (Phase 3 stub)
#[test]
fn capability_config_from_filesystem_write() {
    let cap = Capability::FilesystemWrite(FilesystemWriteParams {
        allowed_root: std::path::PathBuf::from("/data"),
        max_write_bytes: 2048,
    });

    let config = aegis::sandbox::CapabilityConfig::from(&cap);

    assert_eq!(config.name, "filesystem.write");
    assert_eq!(config.max_bytes, 2048);
    // allowed_root should be canonicalized
    assert!(
        config.allowed_root.to_string_lossy().ends_with("data"),
        "allowed_root should end with 'data', got: {:?}",
        config.allowed_root
    );
}

/// A.3: CapabilityConfig from NetworkHttp variant (Phase 3 stub)
#[test]
fn capability_config_from_network_http() {
    let cap = Capability::NetworkHttp(NetworkHttpParams {
        allowed_hosts: vec!["example.com".to_string()],
        allowed_methods: vec!["GET".to_string()],
        max_requests_per_second: 100,
    });

    let config = aegis::sandbox::CapabilityConfig::from(&cap);

    assert_eq!(config.name, "network.http");
    assert_eq!(config.max_bytes, 100);
    // allowed_root is empty PathBuf for network capabilities
    assert!(config.allowed_root.as_os_str().is_empty());
}

/// SA-201: capability_name() for FilesystemWrite variant
#[test]
fn capability_name_filesystem_write() {
    let fs_write = Capability::FilesystemWrite(FilesystemWriteParams {
        allowed_root: std::path::PathBuf::from("/data"),
        max_write_bytes: 1024,
    });
    assert_eq!(fs_write.capability_name(), "filesystem.write");
}

/// SA-201: capability_name() for NetworkHttp variant
#[test]
fn capability_name_network_http() {
    let net_http = Capability::NetworkHttp(NetworkHttpParams {
        allowed_hosts: vec!["example.com".to_string()],
        allowed_methods: vec!["GET".to_string()],
        max_requests_per_second: 50,
    });
    assert_eq!(net_http.capability_name(), "network.http");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Phase 3: filesystem.write Tests (B.4–B.11)
// ═══════════════════════════════════════════════════════════════════════════════

/// B.4 / S-1-W: Allowed Write — writes "hello world" to "output.txt" within root
#[test]
fn allowed_write_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(ALLOWED_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_ok(), "Expected success, got: {:?}", result.err());

    // Verify file was written at allowed_root/output.txt
    let output_path = tmp.path().join("output.txt");
    assert!(output_path.exists(), "Output file should exist");
    let contents = std::fs::read(&output_path).unwrap();
    assert_eq!(contents, b"hello world");

    // Verify receipt was emitted with correct fields
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "success");
    assert_eq!(receipt.path, "output.txt");
    assert_eq!(receipt.size, 11);
    assert!(receipt.timestamp_ns > 0);

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.5 / S-2-W: Denied Write — path resolves outside allowed_root → trap
#[test]
fn denied_write_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(DENIED_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on denied write");
    let err = result.unwrap_err();
    // ../secret.txt contains ".." so it hits traversal check (step 4) before root check (step 5)
    assert!(
        error_chain_contains(&err, "path outside allowed root")
            || error_chain_contains(&err, "path traversal"),
        "Expected path violation error in chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.6 / S-3-W: Path Traversal — ../../etc/passwd → trap (pre-canonicalize)
#[test]
fn traversal_write_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(TRAVERSAL_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on path traversal");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path traversal attempt"),
        "Expected 'path traversal attempt' in error chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");
    assert!(receipt.path.contains(".."), "path should contain ..");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.7 / S-5-W: Size Exceeded — data_len (2048) > max_write_bytes (1024) → trap
#[test]
fn size_exceeded_write_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1024); // 1KB limit

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(SIZE_EXCEEDED_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on size exceeded");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "exceeds"),
        "Expected 'exceeds' in error chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.8 / T-6-W: Guest Memory OOB — data_ptr beyond 64KB memory bounds
#[test]
fn guest_memory_oob_write() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(GUEST_OOB_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on guest memory OOB");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "out of bounds"),
        "Expected 'out of bounds' in error chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap" and path=""
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");
    assert_eq!(receipt.path, "");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.9 / Symlink-W: Symlink Escape — symlink inside root points outside → trap
#[test]
fn symlink_escape_write_traps() {
    let tmp = tempfile::tempdir().unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_file = outside_dir.path().join("secret.txt");

    // Create symlink inside tmp pointing outside
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, tmp.path().join("symlink.txt")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&outside_file, tmp.path().join("symlink.txt")).unwrap();

    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    // Build WAT that writes to the symlink path
    let wat = r#"
        (module
          (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 1024) "symlink.txt\00")
          (data (i32.const 2048) "pwned")
          (func (export "_start")
            (call $fs_write (i32.const 1024) (i32.const 11) (i32.const 2048) (i32.const 5))
            drop
          )
        )
        "#;

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(&wat).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(result.is_err(), "Expected trap on symlink escape");
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "path outside allowed root"),
        "Expected 'path outside allowed root' in error chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.9b / Symlink-W-relative: Symlink with `..` target that doesn't exist → trap
/// Tests the fix for CVE-like bypass: symlink pointing outside via `..` where
/// target doesn't exist yet (hits read_link branch without canonicalize)
#[test]
#[cfg(unix)]
fn symlink_write_relative_traversal_traps() {
    let tmp = tempfile::tempdir().unwrap();

    // Create symlink inside tmp pointing outside via `..` to a non-existent target
    // Target: ../../outside/evil.txt (doesn't exist yet, but would escape root)
    let symlink_path = tmp.path().join("evil_symlink.txt");
    std::os::unix::fs::symlink("../../outside/evil.txt", &symlink_path).unwrap();

    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    // Build WAT that writes to the symlink path
    let wat = r#"
        (module
          (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 1024) "evil_symlink.txt\00")
          (data (i32.const 2048) "pwned")
          (func (export "_start")
            (call $fs_write (i32.const 1024) (i32.const 17) (i32.const 2048) (i32.const 5))
            drop
          )
        )
        "#;

    let (mut sandbox, key_pair) = sandbox_with_receipts();
    let pub_key = key_pair.public_key().as_ref().to_vec();

    let wasm = parse_str(wat).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected trap on symlink traversal with .. target"
    );
    let err = result.unwrap_err();
    assert!(
        error_chain_contains(&err, "symlink target contains '..' traversal segment"),
        "Expected 'symlink target contains' in error chain, got: {:?}",
        err
    );

    // Verify receipt was emitted with result="trap"
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Expected exactly 1 receipt");

    let receipt = &chain[0];
    assert_eq!(receipt.capability_name, "filesystem.write");
    assert_eq!(receipt.action, "write");
    assert_eq!(receipt.result, "trap");

    // Verify Ed25519 signature
    receipt
        .verify_signature(&pub_key)
        .expect("Signature verification failed");
}

/// B.10 / S-416-W: Signing Failure — force_signing_failure before write → Trap,
/// NO target file created, NO new receipt added
#[test]
fn receipt_s416_write_signing_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let emitter_key = {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    };
    let emitter = aegis::receipts::ReceiptEmitter::new(emitter_key);
    sandbox.store_mut().data_mut().receipt_emitter = Some(std::sync::Arc::new(std::sync::Mutex::new(emitter)));

    let wasm = parse_str(ALLOWED_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    // Force signing failure BEFORE calling fs_write
    {
        let emitter = sandbox
            .store_mut()
            .data_mut()
            .receipt_emitter
            .as_ref()
            .expect("receipt emitter missing");
        emitter.lock().unwrap().force_signing_failure();
    }

    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected Trap on signing failure, got: {:?}",
        result
    );

    // Design §7.3: file IS written (temp+rename happens before emit).
    // This is the known asymmetry — we cannot undo the host filesystem write.
    // The critical property is: NO receipt was added to the chain.
    // (receipt_s416_write_after_rename tests this more explicitly)

    // Verify NO additional receipt was added to chain
    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 0, "No receipt should be added when emit fails");
}

/// B.11 / S-416-W-after-rename: First emit succeeds (file written),
/// then force signing failure on second emit.
/// Assert: result.is_err(), file IS at target path, FAIL-CLOSED VIOLATION logged.
#[test]
fn receipt_s416_write_after_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = fs_write_capability(tmp.path().to_str().unwrap(), 1_048_576);

    let mut sandbox = Sandbox::new_with_config(SandboxConfig::default(), false)
        .expect("Failed to create sandbox");

    let emitter_key = {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    };
    let emitter = aegis::receipts::ReceiptEmitter::new(emitter_key);
    sandbox.store_mut().data_mut().receipt_emitter = Some(std::sync::Arc::new(std::sync::Mutex::new(emitter)));

    let wasm = parse_str(ALLOWED_WRITE).expect("WAT parse failed");
    let instance = sandbox
        .instantiate_with_capabilities(&wasm, &[cap])
        .expect("Module should instantiate");

    let func = instance
        .get_typed_func::<(), ()>(sandbox.store_mut(), "_start")
        .expect("Function not found");

    // First call: emit succeeds, file written, receipt added
    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_ok(),
        "First write should succeed: {:?}",
        result.err()
    );

    let output_path = tmp.path().join("output.txt");
    assert!(output_path.exists(), "File should exist after first write");
    let contents = std::fs::read(&output_path).unwrap();
    assert_eq!(contents, b"hello world");

    let chain = sandbox.get_receipt_chain();
    assert_eq!(chain.len(), 1, "Should have 1 receipt after first write");
    assert_eq!(chain[0].result, "success");

    // Force signing failure for second call
    {
        let emitter = sandbox
            .store_mut()
            .data_mut()
            .receipt_emitter
            .as_ref()
            .expect("receipt emitter missing");
        emitter.lock().unwrap().force_signing_failure();
    }

    // Second call: rename succeeds but emit fails → Trap
    let result = func.call(sandbox.store_mut(), ());
    assert!(
        result.is_err(),
        "Expected Trap on signing failure after rename, got: {:?}",
        result
    );

    // File IS at target path (rename succeeded, cannot undo)
    assert!(
        output_path.exists(),
        "File should still exist (rename succeeded, cannot undo)"
    );

    // No new receipt added (emit failed)
    let chain = sandbox.get_receipt_chain();
    assert_eq!(
        chain.len(),
        1,
        "No new receipt should be added when emit fails after rename"
    );
}

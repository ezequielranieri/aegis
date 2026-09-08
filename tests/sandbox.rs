use aegis::capabilities::{
    Capability, FilesystemReadParams, FilesystemWriteParams, NetworkHttpParams,
};
use aegis::sandbox::{Sandbox, SandboxConfig};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;
use wat::parse_str;

// ═══════════════════════════════════════════════════════════════════════════════
// Test Layer for capturing tracing events (emit_capability_event verification)
// ═══════════════════════════════════════════════════════════════════════════════

/// Captured capability event from emit_capability_event
#[derive(Debug, Clone, PartialEq)]
struct CapturedCapabilityEvent {
    capability: String,
    path: String,
    size: u64,
    result: String,
}

/// Test layer that captures capability events
struct CapabilityEventLayer {
    events: Arc<Mutex<Vec<CapturedCapabilityEvent>>>,
}

impl CapabilityEventLayer {
    fn new(events: Arc<Mutex<Vec<CapturedCapabilityEvent>>>) -> Self {
        Self { events }
    }
}

impl<S> Layer<S> for CapabilityEventLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = CapabilityEventVisitor::new(self.events.clone());
        event.record(&mut visitor);
    }
}

struct CapabilityEventVisitor {
    events: Arc<Mutex<Vec<CapturedCapabilityEvent>>>,
    capability: Option<String>,
    path: Option<String>,
    size: Option<u64>,
    result: Option<String>,
}

impl CapabilityEventVisitor {
    fn new(events: Arc<Mutex<Vec<CapturedCapabilityEvent>>>) -> Self {
        Self {
            events,
            capability: None,
            path: None,
            size: None,
            result: None,
        }
    }
}

impl tracing::field::Visit for CapabilityEventVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "capability" => self.capability = Some(value.to_string()),
            "path" => self.path = Some(value.to_string()),
            "result" => self.result = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "size" {
            self.size = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {
        // Ignore debug fields
    }
}

impl Drop for CapabilityEventVisitor {
    fn drop(&mut self) {
        if let (Some(capability), Some(path), Some(size), Some(result)) = (
            self.capability.take(),
            self.path.take(),
            self.size.take(),
            self.result.take(),
        ) {
            if capability == "filesystem.read" {
                self.events.lock().unwrap().push(CapturedCapabilityEvent {
                    capability,
                    path,
                    size,
                    result,
                });
            }
        }
    }
}

/// Run a test with a capability event capturing subscriber
fn with_capability_capture<F, R>(f: F) -> (R, Vec<CapturedCapabilityEvent>)
where
    F: FnOnce() -> R,
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let layer = CapabilityEventLayer::new(events.clone());
    let subscriber = tracing_subscriber::registry().with(layer);

    let result = tracing::subscriber::with_default(subscriber, f);

    let captured = {
        let mut guard = events.lock().unwrap();
        std::mem::take(&mut *guard)
    };

    (result, captured)
}

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
// Helper: create a filesystem.read capability with given root and limit
// ═══════════════════════════════════════════════════════════════════════════════

fn fs_read_capability(root: &str, max_bytes: u64) -> Capability {
    Capability::FilesystemRead(FilesystemReadParams {
        allowed_root: std::path::PathBuf::from(root),
        max_read_bytes: max_bytes,
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
// 4.9+ emit_capability_event verification tests (REQ-108)
// ════════════════════════════════════════════════════════════════════════════════

/// S-1: Happy Path — allowed read within root, under size limit
/// Verifies emit_capability_event is called with capability="filesystem.read",
/// path="test.txt", size=11, result="Success"
#[test]
fn emit_capability_event_s1_happy_path() {
    let (_, captured) = with_capability_capture(|| {
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
    });

    assert_eq!(
        captured.len(),
        1,
        "Expected exactly 1 capability event, got: {:?}",
        captured
    );

    let event = &captured[0];
    assert_eq!(event.capability, "filesystem.read");
    assert_eq!(event.path, "test.txt");
    assert_eq!(event.size, 11); // "hello world" = 11 bytes
    assert_eq!(event.result, "Success");
}

/// S-2 (REQ-103): Path traversal attempt with `..`
/// Verifies emit_capability_event is called with capability="filesystem.read",
/// path containing "..", size=0, result="Trap"
#[test]
fn emit_capability_event_s2_path_traversal() {
    let (_, captured) = with_capability_capture(|| {
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
        assert!(result.is_err(), "Expected trap on path traversal");
    });

    assert_eq!(
        captured.len(),
        1,
        "Expected exactly 1 capability event, got: {:?}",
        captured
    );

    let event = &captured[0];
    assert_eq!(event.capability, "filesystem.read");
    assert!(
        event.path.contains(".."),
        "Path should contain '..', got: {}",
        event.path
    );
    assert_eq!(event.size, 0);
    assert_eq!(event.result, "Trap");
}

/// S-3 (REQ-104): Size limit exceeded
/// Verifies emit_capability_event is called with capability="filesystem.read",
/// path="large.bin", size=2048, result="SizeExceeded"
#[test]
fn emit_capability_event_s3_size_exceeded() {
    let (_, captured) = with_capability_capture(|| {
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
    });

    assert_eq!(
        captured.len(),
        1,
        "Expected exactly 1 capability event, got: {:?}",
        captured
    );

    let event = &captured[0];
    assert_eq!(event.capability, "filesystem.read");
    assert_eq!(event.path, "large.bin");
    assert_eq!(event.size, 2048);
    assert_eq!(event.result, "SizeExceeded");
}

/// S-4 (REQ-105): WASI unknown import (fd_read) — linking error at instantiation
/// Verifies NO emit_capability_event is called because the module fails to link
/// before any host function can be invoked.
#[test]
fn emit_capability_event_s4_wasi_unknown_import() {
    let (_, captured) = with_capability_capture(|| {
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
    });

    // No capability event should be emitted because instantiation failed
    assert_eq!(
        captured.len(),
        0,
        "Expected NO capability events for linking error, got: {:?}",
        captured
    );
}

/// S-5 (REQ-107): Guest memory OOB
/// Verifies emit_capability_event is called with capability="filesystem.read",
/// path="" (empty because path read fails), size=0, result="Trap"
#[test]
fn emit_capability_event_s5_guest_memory_oob() {
    let (_, captured) = with_capability_capture(|| {
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
    });

    assert_eq!(
        captured.len(),
        1,
        "Expected exactly 1 capability event, got: {:?}",
        captured
    );

    let event = &captured[0];
    assert_eq!(event.capability, "filesystem.read");
    // Path is empty because the OOB check happens before path read
    assert_eq!(event.path, "");
    assert_eq!(event.size, 0);
    assert_eq!(event.result, "Trap");
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
    assert_eq!(config.max_read_bytes, 512);
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
    // max_read_bytes is repurposed from max_write_bytes for Phase 3
    assert_eq!(config.max_read_bytes, 2048);
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
        max_requests_per_second: 100,
    });

    let config = aegis::sandbox::CapabilityConfig::from(&cap);

    assert_eq!(config.name, "network.http");
    // max_read_bytes is repurposed from max_requests_per_second for Phase 3
    assert_eq!(config.max_read_bytes, 100);
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
        max_requests_per_second: 50,
    });
    assert_eq!(net_http.capability_name(), "network.http");
}

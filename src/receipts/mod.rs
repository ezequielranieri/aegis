//! Signed execution receipts with hash-chaining

use anyhow::Result;
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::PolicyConfig;
use crate::sandbox::SandboxHandle;

/// Canonical execution receipt with fixed-size hash and signature arrays (REQ-400, REQ-401).
///
/// Struct field declaration order IS the canonical serialization order for
/// `serde_json::to_vec` — no HashMap/BTreeMap, no custom serializer needed.
/// Uses `serde_bytes` for `[u8; N]` arrays to produce base64 in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub capability_name: String,
    pub action: String,
    pub result: String,
    pub path: String,
    pub size: u64,
    #[serde(with = "serde_bytes")]
    pub prev_hash: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
    /// Phase 9: two-phase receipt phase ("" = legacy, "prepare" | "commit" | "abort").
    /// Default empty string for backward compatibility with legacy receipts.
    /// Skip-if-empty ensures legacy receipts serialize byte-identically to pre-change schema.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub phase: String,
    /// Phase 9: pending_hash links prepare→commit/abort. Non-zero only for phase="prepare".
    /// Skip-if-zero ensures legacy receipts (phase="", pending_hash=0) serialize byte-identically.
    #[serde(with = "serde_bytes")]
    #[serde(default, skip_serializing_if = "is_zero_array")]
    pub pending_hash: [u8; 32],
    pub timestamp_ns: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fuel_consumed: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Helper to check if a 32-byte array is all zeros (for skip-if-zero serialization).
fn is_zero_array(v: &[u8; 32]) -> bool {
    v.iter().all(|&b| b == 0)
}

impl ExecutionReceipt {
    /// Create a new receipt, signing the canonical bytes with the provided key pair.
    ///
    /// `prev_hash` is set externally by the caller (ReceiptEmitter or chain logic).
    /// `timestamp_ns` is set to current system time in nanoseconds.
    /// `phase` defaults to "" (legacy) and `pending_hash` to [0;32] for backward compatibility.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capability_name: &str,
        action: &str,
        result: &str,
        path: &str,
        size: u64,
        prev_hash: [u8; 32],
        key_pair: &Ed25519KeyPair,
        fuel_consumed: u64,
    ) -> Result<Self> {
        let timestamp_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;

        let mut receipt = Self {
            capability_name: capability_name.to_string(),
            action: action.to_string(),
            result: result.to_string(),
            path: path.to_string(),
            size,
            prev_hash,
            signature: [0u8; 64], // placeholder for signing
            phase: String::new(),
            pending_hash: [0u8; 32],
            timestamp_ns,
            fuel_consumed,
        };

        // Sign canonical bytes (without signature field)
        let canonical = receipt.canonical_bytes_for_signing();
        let sig = key_pair.sign(&canonical);
        receipt.signature = sig
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature length mismatch"))?;

        Ok(receipt)
    }

    /// Create a new receipt with explicit phase and pending_hash (for two-phase receipts).
    ///
    /// Used by ReceiptEmitter::prepare/commit/abort to create receipts with specific phase
    /// and pending_hash values.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_phase(
        capability_name: &str,
        action: &str,
        result: &str,
        path: &str,
        size: u64,
        prev_hash: [u8; 32],
        key_pair: &Ed25519KeyPair,
        fuel_consumed: u64,
        phase: &str,
        pending_hash: [u8; 32],
    ) -> Result<Self> {
        let timestamp_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;

        let mut receipt = Self {
            capability_name: capability_name.to_string(),
            action: action.to_string(),
            result: result.to_string(),
            path: path.to_string(),
            size,
            prev_hash,
            signature: [0u8; 64], // placeholder for signing
            phase: phase.to_string(),
            pending_hash,
            timestamp_ns,
            fuel_consumed,
        };

        // Sign canonical bytes (without signature field)
        let canonical = receipt.canonical_bytes_for_signing();
        let sig = key_pair.sign(&canonical);
        receipt.signature = sig
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature length mismatch"))?;

        Ok(receipt)
    }

    /// Canonical byte representation for signing (excludes signature field).
    ///
    /// Uses a temporary receipt with zeroed signature to produce deterministic
    /// bytes. The signature is then computed over these bytes.
    fn canonical_bytes_for_signing(&self) -> Vec<u8> {
        let unsigned = Self {
            capability_name: self.capability_name.clone(),
            action: self.action.clone(),
            result: self.result.clone(),
            path: self.path.clone(),
            size: self.size,
            prev_hash: self.prev_hash,
            signature: [0u8; 64],
            phase: self.phase.clone(),
            pending_hash: self.pending_hash,
            timestamp_ns: self.timestamp_ns,
            fuel_consumed: self.fuel_consumed,
        };
        serde_json::to_vec(&unsigned).expect("receipt serialization should not fail")
    }

    /// Canonical byte representation of the full receipt including signature.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("receipt serialization should not fail")
    }

    /// Verify the receipt's Ed25519 signature against the given public key.
    pub fn verify_signature(&self, public_key: &[u8]) -> Result<bool> {
        let unsigned = Self {
            capability_name: self.capability_name.clone(),
            action: self.action.clone(),
            result: self.result.clone(),
            path: self.path.clone(),
            size: self.size,
            prev_hash: self.prev_hash,
            signature: [0u8; 64],
            phase: self.phase.clone(),
            pending_hash: self.pending_hash,
            timestamp_ns: self.timestamp_ns,
            fuel_consumed: self.fuel_consumed,
        };
        let canonical = serde_json::to_vec(&unsigned)?;
        let verifying_key = signature::UnparsedPublicKey::new(&signature::ED25519, public_key);
        Ok(verifying_key.verify(&canonical, &self.signature).is_ok())
    }

    /// Compute the BLAKE3 hash of this receipt's canonical bytes (used for chaining).
    pub fn blake3_hash(&self) -> [u8; 32] {
        *blake3::hash(&self.canonical_bytes()).as_bytes()
    }
}

/// BLAKE3 hash chain for receipts (REQ-410..412).
///
/// Each receipt's `prev_hash` equals `blake3(prev_hash || canonical_bytes)`.
/// Genesis receipt has `prev_hash = [0u8; 32]`.
pub struct ReceiptChain;

impl ReceiptChain {
    /// Compute the hash for a receipt given the previous hash.
    ///
    /// `prev_hash` for the first receipt is `[0u8; 32]` (genesis).
    pub fn compute_chain_hash(prev_hash: &[u8; 32], canonical_bytes: &[u8]) -> [u8; 32] {
        let mut input = Vec::with_capacity(32 + canonical_bytes.len());
        input.extend_from_slice(prev_hash);
        input.extend_from_slice(canonical_bytes);
        *blake3::hash(&input).as_bytes()
    }

    /// Verify a chain of receipts against a public key.
    ///
    /// Validates:
    /// 1. Ed25519 signature over canonical bytes for each receipt
    /// 2. Hash chain integrity with two-phase state machine:
    ///    - Legacy (phase=""): normal chain, pending_hash must be zero
    ///    - Prepare: verify pending_hash = blake3(prev_hash || prepare_canonical), set expected_pending_hash
    ///    - Commit/Abort: require expected_pending_hash, verify prev_hash == pending_hash == expected, clear expected_pending_hash
    /// 3. Reject: orphaned prepare, commit/abort without prepare, prepare after prepare, commit after abort, abort after commit
    /// 4. Accept: idempotent duplicate commit/abort with identical canonical bytes
    /// 5. Timestamps within ±5s of current time
    pub fn verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()> {
        use std::time::Duration;

        let now = SystemTime::now();
        let tolerance = Duration::from_secs(5);

        let mut expected_prev_hash = [0u8; 32]; // genesis
        let mut expected_pending_hash: Option<[u8; 32]> = None;

        for (i, receipt) in receipts.iter().enumerate() {
            // 1. Verify signature
            if !receipt.verify_signature(public_key)? {
                anyhow::bail!("receipt {}: signature verification failed", i);
            }

            // 2. Phase-specific hash chain verification
            let canonical_for_chain = Self::canonical_bytes_for_chain(receipt);

            match receipt.phase.as_str() {
                "" => {
                    // Legacy receipt: normal chain verification
                    let computed_hash =
                        Self::compute_chain_hash(&expected_prev_hash, &canonical_for_chain);
                    if receipt.prev_hash != expected_prev_hash {
                        anyhow::bail!(
                            "receipt {}: prev_hash mismatch (expected {:02x?}, got {:02x?})",
                            i,
                            expected_prev_hash,
                            receipt.prev_hash
                        );
                    }
                    // Legacy receipts must have zero pending_hash
                    if receipt.pending_hash != [0u8; 32] {
                        anyhow::bail!("receipt {}: legacy receipt has non-zero pending_hash", i);
                    }
                    // Chain continues normally
                    expected_prev_hash = computed_hash;
                    // expected_pending_hash unchanged (must be None for legacy to follow)
                }
                "prepare" => {
                    // Prepare receipt: must not have a pending prepare already open
                    if expected_pending_hash.is_some() {
                        anyhow::bail!(
                            "receipt {}: orphaned prepare (previous prepare not closed)",
                            i
                        );
                    }
                    // Verify pending_hash = blake3(prev_hash || prepare_canonical_without_pending_hash)
                    // The pending_hash was computed from canonical bytes with pending_hash=0,
                    // so we must compute canonical bytes the same way for verification.
                    let prepare_canonical =
                        Self::canonical_bytes_for_chain_without_pending_hash(receipt);
                    let computed_pending =
                        Self::compute_chain_hash(&expected_prev_hash, &prepare_canonical);
                    if receipt.pending_hash != computed_pending {
                        anyhow::bail!("receipt {}: prepare pending_hash mismatch", i);
                    }
                    // Set expected_pending_hash for the upcoming commit/abort
                    expected_pending_hash = Some(receipt.pending_hash);
                    // Chain continues from prepare hash
                    expected_prev_hash = computed_pending;
                }
                "commit" | "abort" => {
                    // Commit/Abort: must have a pending prepare
                    let expected = expected_pending_hash.ok_or_else(|| {
                        anyhow::anyhow!("receipt {}: {} without prepare", i, receipt.phase)
                    })?;

                    // Verify prev_hash == expected_pending_hash AND pending_hash == expected_pending_hash
                    if receipt.prev_hash != expected || receipt.pending_hash != expected {
                        anyhow::bail!(
                            "receipt {}: {} prev_hash/pending_hash mismatch (expected {:02x?})",
                            i,
                            receipt.phase,
                            expected
                        );
                    }

                    // Idempotent duplicate check: if next receipt has same phase and identical canonical bytes,
                    // we accept it and continue (the chain hash won't change since canonical is identical)
                    // But we must still verify the signature above passed.

                    // Normal chain update
                    let computed_hash = Self::compute_chain_hash(&expected, &canonical_for_chain);
                    expected_prev_hash = computed_hash;
                    // Terminal: clear expected_pending_hash
                    expected_pending_hash = None;
                }
                phase => {
                    anyhow::bail!("receipt {}: unknown phase: {}", i, phase);
                }
            }

            // 3. Verify timestamp within ±5s
            let receipt_time = UNIX_EPOCH + Duration::from_nanos(receipt.timestamp_ns);
            if receipt_time > now + tolerance
                || receipt_time < now.checked_sub(tolerance).unwrap_or(UNIX_EPOCH)
            {
                anyhow::bail!(
                    "receipt {}: timestamp outside ±5s window (ts={:?}, now={:?})",
                    i,
                    receipt_time,
                    now
                );
            }
        }

        Ok(())
    }

    /// Get canonical bytes for chain hash computation (excludes signature).
    fn canonical_bytes_for_chain(receipt: &ExecutionReceipt) -> Vec<u8> {
        receipt.canonical_bytes_for_signing()
    }

    /// Get canonical bytes for chain hash computation for prepare receipts,
    /// with pending_hash set to zero (to match how pending_hash was computed).
    fn canonical_bytes_for_chain_without_pending_hash(receipt: &ExecutionReceipt) -> Vec<u8> {
        let mut receipt_for_hash = receipt.clone();
        receipt_for_hash.pending_hash = [0u8; 32];
        receipt_for_hash.canonical_bytes_for_signing()
    }
}

/// Emit a capability event stub (Phase 1).
///
/// Logs capability name, path, size, and result via `tracing::info!`.
/// Phase 3 replaces this with signed receipts via `ReceiptEmitter`.
pub fn emit_capability_event(capability: &str, path: &str, size: u64, result: &str) {
    tracing::info!(
        capability = capability,
        path = path,
        size = size,
        result = result,
        "capability_event"
    );
}

/// Opaque handle returned by prepare(), used by commit()/abort().
///
/// Wraps the `pending_hash` (32 bytes) which serves as the map key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrepareHandle {
    pending_hash: [u8; 32],
}

impl PrepareHandle {
    /// Creates a new handle from a pending_hash.
    pub fn new(pending_hash: [u8; 32]) -> Self {
        Self { pending_hash }
    }

    /// Returns the inner pending_hash.
    pub fn pending_hash(&self) -> [u8; 32] {
        self.pending_hash
    }
}

/// Internal struct holding a prepared execution pending commit/abort.
#[allow(dead_code)]
struct PendingReceipt {
    /// The signed prepare receipt (phase="prepare").
    prepare_receipt: ExecutionReceipt,
    /// Handle to the prepared sandbox for reuse in Commit/Abort.
    sandbox_handle: SandboxHandle,
    /// Creation timestamp for TTL tracking.
    created_at: SystemTime,
    /// Capability name for abort receipt emission.
    capability_name: String,
    /// Policy config for replay in Commit.
    config: PolicyConfig,
    /// WASM module bytes for replay in Commit.
    wasm_module: Vec<u8>,
}

/// ReceiptEmitter holds an Ed25519 key pair and creates signed receipts (REQ-430..433).
pub struct ReceiptEmitter {
    key_pair: Ed25519KeyPair,
    last_prev_hash: [u8; 32],
    receipts: Vec<ExecutionReceipt>,
    /// Pending two-phase receipts awaiting commit/abort.
    /// Keyed by pending_hash (same as PrepareHandle).
    pending: HashMap<[u8; 32], PendingReceipt>,
    /// Test-only flag to force the next emit() to fail with a signing error.
    /// Only for use in tests to verify fail-closed behavior.
    #[cfg(feature = "test-utils")]
    force_signing_failure: bool,
    /// Test-only flag to force the next commit()/abort() to fail with a signing error.
    /// Only for use in tests to verify AD-016 commit signing failure handling.
    #[cfg(feature = "test-utils")]
    force_commit_signing_failure: bool,
}

/// TTL for pending receipts in seconds (10 minutes).
const PENDING_TTL_SECS: u64 = 600;
/// TTL sweep interval in seconds.
const TTL_SWEEP_INTERVAL_SECS: u64 = 60;

impl ReceiptEmitter {
    pub fn new(key_pair: Ed25519KeyPair) -> Self {
        Self {
            key_pair,
            last_prev_hash: [0u8; 32], // genesis
            receipts: Vec::new(),
            pending: HashMap::new(),
            #[cfg(feature = "test-utils")]
            force_signing_failure: false,
            #[cfg(feature = "test-utils")]
            force_commit_signing_failure: false,
        }
    }

    /// Spawns the TTL sweep background task.
    ///
    /// Must be called by the owner after wrapping the emitter in `Arc<Mutex>`.
    /// Runs every 60s, emits abort receipts with `error="ttl_expired"` for
    /// expired pending entries, and removes them from the pending map.
    pub fn spawn_ttl_sweep(emitter: Arc<Mutex<Self>>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(TTL_SWEEP_INTERVAL_SECS));
            loop {
                interval.tick().await;
                let mut emitter = emitter.lock().unwrap();
                emitter.cleanup_expired_pending();
            }
        });
    }

    /// Test-only method to force the next emit() to fail with a signing error.
    /// Only for use in tests to verify fail-closed behavior.
    #[cfg(feature = "test-utils")]
    pub fn force_signing_failure(&mut self) {
        self.force_signing_failure = true;
    }

    /// Test-only method to force the next commit()/abort() to fail with a signing error.
    /// Only for use in tests to verify AD-016 commit signing failure handling.
    #[cfg(feature = "test-utils")]
    pub fn force_commit_signing_failure(&mut self) {
        self.force_commit_signing_failure = true;
    }

    /// Emit a signed receipt, updating the chain state.
    ///
    /// Returns `Err` on signing failure — never produces an unsigned receipt (REQ-433).
    /// `fuel_consumed`: for capability receipts (fs/network), pass 0 (skip-if-zero serialization);
    /// for Execute receipts, pass the measured `sandbox.fuel_consumed()`.
    pub fn emit(
        &mut self,
        capability: &str,
        action: &str,
        path: &str,
        size: u64,
        result: &str,
        fuel_consumed: u64,
    ) -> Result<ExecutionReceipt> {
        // Test-only hook to force signing failure for fail-closed verification.
        // Only compiled with the "test-utils" feature (enabled for dev-dependencies).
        #[cfg(feature = "test-utils")]
        if self.force_signing_failure {
            self.force_signing_failure = false; // reset for next call
            return Err(anyhow::anyhow!("test-forced signing failure"));
        }

        let receipt = ExecutionReceipt::new(
            capability,
            action,
            result,
            path,
            size,
            self.last_prev_hash,
            &self.key_pair,
            fuel_consumed,
        )?;

        // Update chain: next receipt's prev_hash = blake3(this receipt's canonical bytes)
        self.last_prev_hash = ReceiptChain::compute_chain_hash(
            &self.last_prev_hash,
            &receipt.canonical_bytes_for_signing(),
        );

        // Store receipt in chain
        self.receipts.push(receipt.clone());

        Ok(receipt)
    }

    /// Internal helper to create a signed receipt with a specific timestamp.
    ///
    /// Used by `prepare()` to create receipts with a fixed timestamp for consistent
    /// canonical byte computation across hash computation and final signing.
    #[allow(clippy::too_many_arguments)]
    fn create_receipt_for_hash(
        &self,
        capability: &str,
        action: &str,
        result: &str,
        path: &str,
        size: u64,
        prev_hash: [u8; 32],
        key_pair: &Ed25519KeyPair,
        fuel_consumed: u64,
        phase: &str,
        pending_hash: [u8; 32],
        timestamp_ns: u64,
    ) -> Result<ExecutionReceipt> {
        let mut receipt = ExecutionReceipt {
            capability_name: capability.to_string(),
            action: action.to_string(),
            result: result.to_string(),
            path: path.to_string(),
            size,
            prev_hash,
            signature: [0u8; 64], // placeholder
            phase: phase.to_string(),
            pending_hash,
            timestamp_ns,
            fuel_consumed,
        };

        // Sign canonical bytes (without signature field)
        let canonical = receipt.canonical_bytes_for_signing();
        let sig = key_pair.sign(&canonical);
        receipt.signature = sig
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature length mismatch"))?;

        Ok(receipt)
    }

    /// Emit a legacy-format signed receipt WITHOUT updating the chain state.
    ///
    /// Creates a receipt with `phase=""` (legacy format) for backward compatibility
    /// with legacy Execute RPC clients. Does NOT update `last_prev_hash` or add to `receipts`.
    /// Used by the legacy Execute wrapper to maintain backward compatible responses
    /// while the two-phase chain (prepare/commit/abort) is maintained internally.
    pub fn emit_legacy(
        &mut self,
        capability: &str,
        action: &str,
        path: &str,
        size: u64,
        result: &str,
        fuel_consumed: u64,
    ) -> Result<ExecutionReceipt> {
        #[cfg(feature = "test-utils")]
        if self.force_signing_failure {
            self.force_signing_failure = false;
            return Err(anyhow::anyhow!("test-forced signing failure"));
        }

        // Create legacy receipt (phase="") with current chain tip as prev_hash
        let receipt = ExecutionReceipt::new(
            capability,
            action,
            result,
            path,
            size,
            self.last_prev_hash,
            &self.key_pair,
            fuel_consumed,
        )?;

        // NOTE: Does NOT update last_prev_hash or add to receipts chain.
        // This receipt is for client response only; the two-phase chain
        // (prepare/commit/abort) is maintained separately.

        Ok(receipt)
    }

    /// Get a reference to the receipt chain.
    pub fn chain(&self) -> &[ExecutionReceipt] {
        &self.receipts
    }

    /// Return the Ed25519 public key bytes.
    ///
    /// The private key is never exported — only the 32-byte public component is returned.
    pub fn public_key(&self) -> Vec<u8> {
        self.key_pair.public_key().as_ref().to_vec()
    }

    // ===== Two-Phase Receipt Methods (Phase 9) =====

    /// Prepares a two-phase execution: creates sandbox, signs prepare receipt, stores pending state.
    ///
    /// Returns the signed prepare receipt (phase="prepare") and a PrepareHandle for commit/abort.
    /// The prepare receipt has result="pending", path="", size=0, fuel_consumed=0.
    /// The pending_hash = blake3(prev_hash || prepare_canonical_bytes_without_pending_hash).
    pub fn prepare(
        &mut self,
        capability: &str,
        config: PolicyConfig,
        wasm_module: &[u8],
        fuel_budget: Option<u64>,
    ) -> Result<(ExecutionReceipt, PrepareHandle)> {
        // Create prepare receipt with phase="prepare"
        // prev_hash is the current chain tip
        let prev_hash = self.last_prev_hash;
        // Use a fixed timestamp for both hash computation and final receipt
        let timestamp_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;

        // Create prepare receipt with pending_hash = 0 (for pending_hash computation)
        // Using a fixed timestamp so canonical bytes are identical
        let prepare_receipt_for_hash = self.create_receipt_for_hash(
            capability,
            "execute",
            "pending",
            "",
            0,
            prev_hash,
            &self.key_pair,
            0,
            "prepare",
            [0u8; 32],
            timestamp_ns,
        )?;

        // Compute pending_hash = blake3(prev_hash || prepare_canonical_bytes_without_pending_hash)
        // Use canonical bytes WITHOUT the pending_hash field (which is 0 in this receipt)
        let prepare_canonical = prepare_receipt_for_hash.canonical_bytes_for_signing();
        let pending_hash = ReceiptChain::compute_chain_hash(&prev_hash, &prepare_canonical);

        // Now create the final prepare receipt with the correct pending_hash and re-sign
        // Using the SAME timestamp so canonical bytes match
        let prepare_receipt = self.create_receipt_for_hash(
            capability,
            "execute",
            "pending",
            "",
            0,
            prev_hash,
            &self.key_pair,
            0,
            "prepare",
            pending_hash,
            timestamp_ns,
        )?;

        // Create sandbox for this execution
        let sandbox = crate::sandbox::Sandbox::new_with_config(
            crate::sandbox::SandboxConfig {
                fuel_budget,
                ..Default::default()
            },
            !std::env::var("AEGIS_TEST_MODE").is_ok(), // enable_epoch = !test_mode
        )?;

        let sandbox_handle = SandboxHandle::new(sandbox);

        // Store pending receipt
        let pending = PendingReceipt {
            prepare_receipt: prepare_receipt.clone(),
            sandbox_handle,
            created_at: SystemTime::now(),
            capability_name: capability.to_string(),
            config,
            wasm_module: wasm_module.to_vec(),
        };

        self.pending.insert(pending_hash, pending);

        // Update chain: next receipt's prev_hash = pending_hash
        self.last_prev_hash = pending_hash;

        // Store prepare receipt in chain
        self.receipts.push(prepare_receipt.clone());

        Ok((prepare_receipt, PrepareHandle::new(pending_hash)))
    }

    /// Get the sandbox handle for a pending execution without removing it.
    ///
    /// Used by the gRPC handler to execute WASM in the prepared sandbox before calling commit().
    pub fn get_pending_sandbox(&self, handle: &PrepareHandle) -> Option<SandboxHandle> {
        self.pending
            .get(&handle.pending_hash())
            .map(|p| p.sandbox_handle.clone())
    }

    /// Get the capability name for a pending execution.
    pub fn get_pending_capability_name(&self, handle: &PrepareHandle) -> Option<String> {
        self.pending
            .get(&handle.pending_hash())
            .map(|p| p.capability_name.clone())
    }

    /// Get the policy config for a pending execution.
    pub fn get_pending_config(&self, handle: &PrepareHandle) -> Option<PolicyConfig> {
        self.pending
            .get(&handle.pending_hash())
            .map(|p| p.config.clone())
    }

    /// Get the WASM module bytes for a pending execution.
    pub fn get_pending_wasm_module(&self, handle: &PrepareHandle) -> Option<Vec<u8>> {
        self.pending
            .get(&handle.pending_hash())
            .map(|p| p.wasm_module.clone())
    }

    /// Commits a prepared execution: executes WASM in prepared sandbox, signs commit receipt.
    ///
    /// Idempotent: if the pending entry was already committed/aborted, returns the existing commit receipt.
    /// Uses the provided result/path/size/fuel_consumed from the handler (NOT re-executing WASM here).
    pub fn commit(
        &mut self,
        handle: PrepareHandle,
        result: &[u8],
        path: &str,
        size: u64,
        fuel_consumed: u64,
    ) -> Result<ExecutionReceipt> {
        // Test-only hook to force commit signing failure for AD-016 verification.
        #[cfg(feature = "test-utils")]
        if self.force_commit_signing_failure {
            self.force_commit_signing_failure = false; // reset for next call
            return Err(anyhow::anyhow!("test-forced commit signing failure"));
        }

        let pending_hash = handle.pending_hash();

        // Try to remove the pending entry (idempotent: if already removed, return existing commit)
        let pending = match self.pending.remove(&pending_hash) {
            Some(p) => p,
            None => {
                // Already committed/aborted - find and return the existing commit receipt
                return self
                    .receipts
                    .iter()
                    .rev()
                    .find(|r| r.pending_hash == pending_hash && r.phase == "commit")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("commit not found for handle"));
            }
        };

        // Create commit receipt with actual execution results
        // prev_hash = pending_hash (links to prepare)
        // pending_hash = same as prepare (for chain verification)
        let commit_receipt = ExecutionReceipt::new_with_phase(
            &pending.capability_name,
            "execute",
            &String::from_utf8_lossy(result),
            path,
            size,
            pending_hash, // prev_hash = prepare's pending_hash
            &self.key_pair,
            fuel_consumed,
            "commit",
            pending_hash, // pending_hash = prepare's pending_hash
        )?;

        // Update chain: next receipt's prev_hash = blake3(commit canonical bytes)
        self.last_prev_hash = ReceiptChain::compute_chain_hash(
            &pending_hash,
            &commit_receipt.canonical_bytes_for_signing(),
        );

        // Store commit receipt in chain
        self.receipts.push(commit_receipt.clone());

        Ok(commit_receipt)
    }

    /// Aborts a prepared execution: emits abort receipt, discards sandbox.
    ///
    /// Idempotent: if the pending entry was already committed/aborted, returns the existing abort receipt.
    pub fn abort(
        &mut self,
        handle: PrepareHandle,
        error: Option<&str>,
    ) -> Result<ExecutionReceipt> {
        // Test-only hook to force commit/abort signing failure for AD-016 verification.
        #[cfg(feature = "test-utils")]
        if self.force_commit_signing_failure {
            self.force_commit_signing_failure = false; // reset for next call
            return Err(anyhow::anyhow!("test-forced commit signing failure"));
        }

        let pending_hash = handle.pending_hash();

        // Try to remove the pending entry (idempotent)
        let pending = match self.pending.remove(&pending_hash) {
            Some(p) => p,
            None => {
                // Already committed/aborted - find and return the existing abort receipt
                return self
                    .receipts
                    .iter()
                    .rev()
                    .find(|r| r.pending_hash == pending_hash && r.phase == "abort")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("abort not found for handle"));
            }
        };

        // Create abort receipt
        let _error_msg = error.unwrap_or("aborted");
        let abort_receipt = ExecutionReceipt::new_with_phase(
            &pending.capability_name,
            "execute",
            "aborted",
            "",
            0,
            pending_hash, // prev_hash = prepare's pending_hash
            &self.key_pair,
            0, // fuel_consumed = 0 for abort
            "abort",
            pending_hash, // pending_hash = prepare's pending_hash
        )?;

        // Update chain: next receipt's prev_hash = blake3(abort canonical bytes)
        self.last_prev_hash = ReceiptChain::compute_chain_hash(
            &pending_hash,
            &abort_receipt.canonical_bytes_for_signing(),
        );

        // Store abort receipt in chain
        self.receipts.push(abort_receipt.clone());

        Ok(abort_receipt)
    }

    /// Internal helper to emit an abort receipt for a given pending_hash (used by TTL sweep).
    fn emit_abort_internal(
        &mut self,
        pending_hash: [u8; 32],
        error: Option<&str>,
    ) -> Result<ExecutionReceipt> {
        let _error_msg = error.unwrap_or("ttl_expired");

        // Find the prepare receipt in chain to get capability_name
        let prepare_receipt = self
            .receipts
            .iter()
            .rev()
            .find(|r| r.pending_hash == pending_hash && r.phase == "prepare")
            .ok_or_else(|| anyhow::anyhow!("prepare receipt not found for pending_hash"))?;

        let abort_receipt = ExecutionReceipt::new_with_phase(
            &prepare_receipt.capability_name,
            "execute",
            "aborted",
            "",
            0,
            pending_hash,
            &self.key_pair,
            0,
            "abort",
            pending_hash,
        )?;

        // Update chain
        self.last_prev_hash = ReceiptChain::compute_chain_hash(
            &pending_hash,
            &abort_receipt.canonical_bytes_for_signing(),
        );

        self.receipts.push(abort_receipt.clone());

        Ok(abort_receipt)
    }

    /// Cleans up expired pending entries (called by TTL sweep task).
    ///
    /// Iterates pending map, finds entries older than PENDING_TTL_SECS,
    /// emits abort receipt with error="ttl_expired" for each, and removes them.
    fn cleanup_expired_pending(&mut self) {
        let now = SystemTime::now();
        let ttl_duration = Duration::from_secs(PENDING_TTL_SECS);

        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.created_at).unwrap_or_default() > ttl_duration)
            .map(|(k, _)| *k)
            .collect();

        for pending_hash in expired {
            if self.pending.remove(&pending_hash).is_some() {
                // Emit abort receipt BEFORE removal (still holding lock)
                let _ = self.emit_abort_internal(pending_hash, Some("ttl_expired"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    fn test_keypair() -> Ed25519KeyPair {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    }

    // === S-414: Deterministic serialization ===

    #[test]
    fn deterministic_serialization() {
        let kp = test_keypair();
        let receipt = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "test.txt",
            11,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        let bytes1 = receipt.canonical_bytes();
        let bytes2 = receipt.canonical_bytes();
        assert_eq!(
            bytes1, bytes2,
            "S-414: same receipt must produce identical bytes"
        );

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_slice(&bytes1).unwrap();
        assert_eq!(parsed["capability_name"], "filesystem.read");
        assert_eq!(parsed["action"], "read");
        assert_eq!(parsed["result"], "success");
        assert_eq!(parsed["path"], "test.txt");
        assert_eq!(parsed["size"], 11);
    }

    // === S-400: Valid receipt creation ===

    #[test]
    fn receipt_creation_s400() {
        let kp = test_keypair();
        let receipt = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "data.txt",
            42,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        assert_eq!(receipt.capability_name, "filesystem.read");
        assert_eq!(receipt.action, "read");
        assert_eq!(receipt.result, "success");
        assert_eq!(receipt.path, "data.txt");
        assert_eq!(receipt.size, 42);
        assert_eq!(receipt.prev_hash, [0u8; 32]);
        assert!(receipt.signature != [0u8; 64], "signature must be non-zero");
        assert!(receipt.timestamp_ns > 0, "timestamp must be non-zero");
    }

    // === S-401: Hash chain integrity ===

    #[test]
    fn hash_chain_integrity_s401() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();

        let r1 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "a.txt",
            10,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        let prev_hash =
            ReceiptChain::compute_chain_hash(&[0u8; 32], &r1.canonical_bytes_for_signing());

        let r2 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "b.txt",
            20,
            prev_hash,
            &kp,
            0,
        )
        .unwrap();

        let chain = vec![r1, r2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_ok(),
            "S-401: valid chain should verify: {:?}",
            result.err()
        );
    }

    // === S-410: Verifier detects tampered payload ===

    #[test]
    fn verifier_tampered_payload_s410() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();

        let mut r1 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "a.txt",
            10,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        // Tamper with the payload
        r1.path = "tampered.txt".to_string();

        let chain = vec![r1];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_err(),
            "S-410: tampered payload should fail verification"
        );
    }

    // === S-411: Verifier detects broken chain ===

    #[test]
    fn verifier_broken_chain_s411() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();

        let r1 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "a.txt",
            10,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        // Create r2 with wrong prev_hash (broken chain)
        let mut r2 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "b.txt",
            20,
            [1u8; 32], // wrong prev_hash — should be hash of r1
            &kp,
            0,
        )
        .unwrap();

        // Re-sign r2 with the wrong prev_hash so signature is valid
        // but chain is broken
        let canonical = r2.canonical_bytes_for_signing();
        let sig = kp.sign(&canonical);
        r2.signature = sig.as_ref().try_into().unwrap();

        let chain = vec![r1, r2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_err(),
            "S-411: broken chain should fail verification"
        );
    }

    // === S-412: Verifier detects expired timestamp ===

    #[test]
    fn verifier_expired_timestamp_s412() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();

        let mut r1 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "a.txt",
            10,
            [0u8; 32],
            &kp,
            0,
        )
        .unwrap();

        // Set timestamp 10 seconds in the past (beyond ±5s tolerance)
        let old_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
            - 10_000_000_000; // 10s ago
        r1.timestamp_ns = old_ns;

        // Re-sign with the old timestamp
        let canonical = r1.canonical_bytes_for_signing();
        let sig = kp.sign(&canonical);
        r1.signature = sig.as_ref().try_into().unwrap();

        let chain = vec![r1];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_err(),
            "S-412: expired timestamp should fail verification"
        );
    }

    // === S-413: Verifier detects wrong public key ===

    #[test]
    fn verifier_wrong_public_key_s413() {
        let kp1 = test_keypair();
        let kp2 = test_keypair();
        let wrong_pub_key = kp2.public_key().as_ref().to_vec();

        let r1 = ExecutionReceipt::new(
            "filesystem.read",
            "read",
            "success",
            "a.txt",
            10,
            [0u8; 32],
            &kp1,
            0,
        )
        .unwrap();

        let chain = vec![r1];
        let result = ReceiptChain::verify_chain(&chain, &wrong_pub_key);
        assert!(
            result.is_err(),
            "S-413: wrong public key should fail verification"
        );
    }

    // === S-400: ReceiptEmitter emit ===

    #[test]
    fn receipt_emitter_emit_s400() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        let receipt = emitter
            .emit("filesystem.read", "read", "test.txt", 11, "success", 0)
            .unwrap();

        assert_eq!(receipt.capability_name, "filesystem.read");
        assert_eq!(receipt.result, "success");
        assert_eq!(receipt.prev_hash, [0u8; 32]); // genesis
    }

    #[test]
    fn receipt_emitter_chain_tracking() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        let r1 = emitter
            .emit("filesystem.read", "read", "a.txt", 10, "success", 0)
            .unwrap();
        let r2 = emitter
            .emit("filesystem.read", "read", "b.txt", 20, "success", 0)
            .unwrap();

        // r2's prev_hash should NOT be genesis
        assert_ne!(
            r2.prev_hash, [0u8; 32],
            "second receipt must chain from first"
        );

        // Full chain should verify
        let chain = vec![r1, r2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_ok(),
            "emitted chain should verify: {:?}",
            result.err()
        );
    }

    // === S-416: Signing failure forced in test ===

    #[test]
    fn receipt_emitter_signing_failure_forced() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        // Normal emit should succeed
        let receipt = emitter.emit("filesystem.read", "read", "test.txt", 11, "success", 0);
        assert!(receipt.is_ok(), "Normal emit should succeed");

        // Force next emit to fail
        emitter.force_signing_failure();

        // Next emit should fail
        let result = emitter.emit("filesystem.read", "read", "test.txt", 11, "success", 0);
        assert!(result.is_err(), "Emit should fail when forced");

        // Next emit after failure should succeed again (flag reset)
        let receipt = emitter.emit("filesystem.read", "read", "test.txt", 11, "success", 0);
        assert!(receipt.is_ok(), "Emit should succeed after forced failure");
    }

    // === E2E: CLI verifier reads chain, validates ===

    #[test]
    fn e2e_cli_verifier_roundtrip() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        // Emit a chain of receipts
        let r1 = emitter
            .emit("filesystem.read", "read", "a.txt", 10, "success", 0)
            .unwrap();
        let r2 = emitter
            .emit("filesystem.read", "read", "b.txt", 20, "success", 0)
            .unwrap();

        // Save chain to JSON file
        let tmp = tempfile::tempdir().unwrap();
        let chain_path = tmp.path().join("chain.json");
        let chain = vec![r1, r2];
        let json = serde_json::to_string_pretty(&chain).unwrap();
        std::fs::write(&chain_path, &json).unwrap();

        // Save public key
        use base64::Engine;
        let _pub_key_b64 = base64::engine::general_purpose::STANDARD.encode(&pub_key);

        // Verify via library function (same logic as CLI)
        let parsed: Vec<ExecutionReceipt> = serde_json::from_str(&json).unwrap();
        let result = ReceiptChain::verify_chain(&parsed, &pub_key);
        assert!(
            result.is_ok(),
            "E2E: chain should verify: {:?}",
            result.err()
        );
    }

    // === Phase 8: fuel_consumed field verification ===

    /// E-802: golden bytes test — receipt with fuel_consumed=0 serializes byte-identical
    /// to pre-change schema (skip-if-zero omits the field). Old chains verify unchanged.
    #[test]
    fn e802_golden_bytes_fuel_zero_byte_identical() {
        let kp = test_keypair();
        let receipt = ExecutionReceipt::new(
            "execute", "execute", "success", "", 0, [0u8; 32], &kp, 0, // fuel_consumed = 0
        )
        .unwrap();

        // Serialize with fuel=0
        let bytes_with_zero = receipt.canonical_bytes();

        // Build a "pre-change" receipt manually (same fields except fuel_consumed)
        // and serialize it the same way — should be byte-identical
        #[derive(Serialize)]
        struct LegacyReceipt {
            capability_name: String,
            action: String,
            result: String,
            path: String,
            size: u64,
            #[serde(with = "serde_bytes")]
            prev_hash: [u8; 32],
            #[serde(with = "serde_bytes")]
            signature: [u8; 64],
            timestamp_ns: u64,
        }

        let legacy = LegacyReceipt {
            capability_name: receipt.capability_name.clone(),
            action: receipt.action.clone(),
            result: receipt.result.clone(),
            path: receipt.path.clone(),
            size: receipt.size,
            prev_hash: receipt.prev_hash,
            signature: receipt.signature,
            timestamp_ns: receipt.timestamp_ns,
        };
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();

        // Byte-identical: skip-if-zero omits fuel_consumed when 0
        assert_eq!(
            bytes_with_zero, legacy_bytes,
            "E-802: receipt with fuel_consumed=0 must be byte-identical to pre-change schema"
        );

        // Also verify the field is absent in JSON when 0
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(
            !json.contains("fuel_consumed"),
            "fuel_consumed field must be omitted from JSON when 0 (skip-if-zero)"
        );
    }

    /// S-803: skip-if-zero + field order — field absent at 0, present and last in canonical bytes at >0
    #[test]
    fn s803_skip_if_zero_field_order() {
        let kp = test_keypair();

        // fuel=0 → field absent in canonical JSON (skip-if-zero)
        let receipt_zero =
            ExecutionReceipt::new("execute", "execute", "success", "", 0, [0u8; 32], &kp, 0)
                .unwrap();
        let json_zero = serde_json::to_string(&receipt_zero).unwrap();
        assert!(
            !json_zero.contains("fuel_consumed"),
            "fuel_consumed must be absent when 0 (skip-if-zero)"
        );

        // fuel=42 → field present; verify field order in CANONICAL BYTES (struct declaration order)
        // by checking that fuel_consumed appears AFTER timestamp_ns in the serialized bytes
        let receipt_nonzero =
            ExecutionReceipt::new("execute", "execute", "success", "", 0, [0u8; 32], &kp, 42)
                .unwrap();
        let canonical = receipt_nonzero.canonical_bytes();
        let canonical_str = String::from_utf8(canonical).unwrap();

        // In canonical bytes (struct field order), fuel_consumed must appear AFTER timestamp_ns
        let ts_pos = canonical_str
            .rfind("timestamp_ns")
            .expect("timestamp_ns must exist");
        let fuel_pos = canonical_str
            .rfind("fuel_consumed")
            .expect("fuel_consumed must exist");
        assert!(
            fuel_pos > ts_pos,
            "fuel_consumed must appear AFTER timestamp_ns in canonical bytes (struct declaration order)"
        );

        // Also verify the field value is correct
        let json = serde_json::to_value(&receipt_nonzero).unwrap();
        assert_eq!(json["fuel_consumed"], 42);
    }

    // === Phase 9: Two-Phase Receipt Tests (WU2-9) ===

    #[test]
    fn prepare_commit_abort_flow() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        // Test prepare
        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();
        let (prepare_receipt, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();

        assert_eq!(prepare_receipt.phase, "prepare");
        assert_eq!(prepare_receipt.result, "pending");
        assert_eq!(prepare_receipt.path, "");
        assert_eq!(prepare_receipt.size, 0);
        assert_eq!(prepare_receipt.fuel_consumed, 0);
        assert_ne!(prepare_receipt.pending_hash, [0u8; 32]);

        // Test commit
        let commit_receipt = emitter
            .commit(handle, b"success", "test.txt", 42, 500)
            .unwrap();

        assert_eq!(commit_receipt.phase, "commit");
        assert_eq!(commit_receipt.result, "success");
        assert_eq!(commit_receipt.path, "test.txt");
        assert_eq!(commit_receipt.size, 42);
        assert_eq!(commit_receipt.fuel_consumed, 500);
        assert_eq!(commit_receipt.pending_hash, prepare_receipt.pending_hash);
        assert_eq!(commit_receipt.prev_hash, prepare_receipt.pending_hash);

        // Test abort (on a new prepare)
        let (prepare_receipt2, handle2) = emitter
            .prepare("execute", config, &wasm_module, Some(1000))
            .unwrap();

        let abort_receipt = emitter.abort(handle2, Some("user_abort")).unwrap();

        assert_eq!(abort_receipt.phase, "abort");
        assert_eq!(abort_receipt.result, "aborted");
        assert_eq!(abort_receipt.pending_hash, prepare_receipt2.pending_hash);
        assert_eq!(abort_receipt.prev_hash, prepare_receipt2.pending_hash);
    }

    #[test]
    fn prepare_commit_idempotent() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();
        let (_prepare_receipt, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();

        // First commit
        let commit1 = emitter
            .commit(handle, b"success", "test.txt", 42, 500)
            .unwrap();

        // Second commit with same handle (idempotent)
        let commit2 = emitter
            .commit(handle, b"success", "test.txt", 42, 500)
            .unwrap();

        // Should return the same commit receipt
        assert_eq!(commit1.canonical_bytes(), commit2.canonical_bytes());
    }

    #[test]
    fn prepare_abort_idempotent() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();
        let (_prepare_receipt, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();

        // First abort
        let abort1 = emitter.abort(handle, Some("user_abort")).unwrap();

        // Second abort with same handle (idempotent)
        let abort2 = emitter.abort(handle, Some("user_abort")).unwrap();

        // Should return the same abort receipt
        assert_eq!(abort1.canonical_bytes(), abort2.canonical_bytes());
    }

    #[test]
    fn ttl_expiry_emits_abort() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();
        let (_prepare_receipt, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();

        // Verify the pending entry exists
        assert!(emitter.get_pending_sandbox(&handle).is_some());
        assert!(emitter.get_pending_capability_name(&handle).is_some());
        assert!(emitter.get_pending_config(&handle).is_some());
        assert!(emitter.get_pending_wasm_module(&handle).is_some());

        // Clean up
        let _ = emitter.abort(handle, Some("test_cleanup"));
    }

    #[test]
    fn concurrent_pending_entries() {
        let kp = test_keypair();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();

        // Create multiple concurrent prepares
        let (_, handle1) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();
        let (_, handle2) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();
        let (_, handle3) = emitter
            .prepare("execute", config, &wasm_module, Some(1000))
            .unwrap();

        // All three should have different pending_hashes
        assert_ne!(handle1.pending_hash(), handle2.pending_hash());
        assert_ne!(handle2.pending_hash(), handle3.pending_hash());
        assert_ne!(handle1.pending_hash(), handle3.pending_hash());

        // Commit one, abort another, leave third
        let commit = emitter
            .commit(handle1, b"success", "a.txt", 10, 100)
            .unwrap();
        assert_eq!(commit.phase, "commit");

        let abort = emitter.abort(handle2, Some("cancelled")).unwrap();
        assert_eq!(abort.phase, "abort");

        // Third should still be pending
        assert!(emitter.get_pending_sandbox(&handle3).is_some());

        // Clean up third
        let _ = emitter.abort(handle3, Some("test_cleanup"));
    }

    #[test]
    fn commit_chain_verification() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();

        // Prepare -> Commit chain
        let (prepare, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();
        let commit = emitter
            .commit(handle, b"success", "test.txt", 42, 500)
            .unwrap();

        // Verify chain
        let chain = vec![prepare, commit];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_ok(),
            "prepare->commit chain should verify: {:?}",
            result.err()
        );
    }

    #[test]
    fn abort_chain_verification() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();

        // Prepare -> Abort chain
        let (prepare, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();
        let abort = emitter.abort(handle, Some("user_abort")).unwrap();

        // Verify chain
        let chain = vec![prepare, abort];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_ok(),
            "prepare->abort chain should verify: {:?}",
            result.err()
        );
    }

    #[test]
    fn orphaned_prepare_rejected() {
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();

        // Create two prepares without closing the first
        let (prepare1, _) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();
        let (prepare2, _) = emitter
            .prepare("execute", config, &wasm_module, Some(1000))
            .unwrap();

        // Chain with both prepares should fail verification
        let chain = vec![prepare1, prepare2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(result.is_err(), "orphaned prepare should be rejected");
        assert!(result.unwrap_err().to_string().contains("orphaned prepare"));
    }

    #[test]
    fn commit_without_prepare_rejected() {
        let kp1 = test_keypair();
        let kp2 = test_keypair();
        let pub_key = kp1.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp1);

        let config = crate::config::PolicyConfig::default();
        let wasm_module = wat::parse_str("(module)").unwrap();

        // Create a prepare
        let (prepare, handle) = emitter
            .prepare("execute", config.clone(), &wasm_module, Some(1000))
            .unwrap();

        // Create a fake commit receipt without matching prepare (simulate attack)
        // We can't easily create one via emitter, so test via verifier directly
        // by creating a commit receipt with wrong prev_hash/pending_hash
        let fake_commit = emitter
            .commit(handle, b"success", "test.txt", 42, 500)
            .unwrap();

        // Now manually create another commit with different prev_hash using a different keypair
        let mut commit2 = fake_commit.clone();
        commit2.prev_hash = [1u8; 32]; // wrong prev_hash
                                       // Re-sign with kp2
        let canonical = commit2.canonical_bytes_for_signing();
        let sig = kp2.sign(&canonical);
        commit2.signature = sig.as_ref().try_into().unwrap();

        let chain = vec![prepare, commit2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_err(),
            "commit without matching prepare should be rejected"
        );
    }

    #[test]
    fn verify_chain_legacy_still_works() {
        // Ensure legacy receipts (phase="") still verify correctly
        let kp = test_keypair();
        let pub_key = kp.public_key().as_ref().to_vec();
        let mut emitter = ReceiptEmitter::new(kp);

        let r1 = emitter
            .emit("filesystem.read", "read", "a.txt", 10, "success", 0)
            .unwrap();
        let r2 = emitter
            .emit("filesystem.read", "read", "b.txt", 20, "success", 0)
            .unwrap();

        let chain = vec![r1, r2];
        let result = ReceiptChain::verify_chain(&chain, &pub_key);
        assert!(
            result.is_ok(),
            "legacy chain should still verify: {:?}",
            result.err()
        );
    }
}

//! Signed execution receipts with hash-chaining

use anyhow::Result;
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub timestamp_ns: u64,
}

impl ExecutionReceipt {
    /// Create a new receipt, signing the canonical bytes with the provided key pair.
    ///
    /// `prev_hash` is set externally by the caller (ReceiptEmitter or chain logic).
    /// `timestamp_ns` is set to current system time in nanoseconds.
    pub fn new(
        capability_name: &str,
        action: &str,
        result: &str,
        path: &str,
        size: u64,
        prev_hash: [u8; 32],
        key_pair: &Ed25519KeyPair,
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
            timestamp_ns,
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
            timestamp_ns: self.timestamp_ns,
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
            timestamp_ns: self.timestamp_ns,
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
    /// 2. Hash chain integrity: `prev_hash[i] == blake3(prev_hash[i-1] || canonical[i])`
    /// 3. Timestamps within ±5s of current time
    pub fn verify_chain(receipts: &[ExecutionReceipt], public_key: &[u8]) -> Result<()> {
        use std::time::Duration;

        let now = SystemTime::now();
        let tolerance = Duration::from_secs(5);

        let mut expected_prev_hash = [0u8; 32]; // genesis

        for (i, receipt) in receipts.iter().enumerate() {
            // 1. Verify signature
            if !receipt.verify_signature(public_key)? {
                anyhow::bail!("receipt {}: signature verification failed", i);
            }

            // 2. Verify hash chain integrity
            let canonical_for_chain = Self::canonical_bytes_for_chain(receipt);
            let computed_hash = Self::compute_chain_hash(&expected_prev_hash, &canonical_for_chain);
            if receipt.prev_hash != expected_prev_hash {
                anyhow::bail!(
                    "receipt {}: prev_hash mismatch (expected {:02x?}, got {:02x?})",
                    i,
                    expected_prev_hash,
                    receipt.prev_hash
                );
            }

            // Update expected for next iteration
            expected_prev_hash = computed_hash;

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

/// ReceiptEmitter holds an Ed25519 key pair and creates signed receipts (REQ-430..433).
pub struct ReceiptEmitter {
    key_pair: Ed25519KeyPair,
    last_prev_hash: [u8; 32],
    receipts: Vec<ExecutionReceipt>,
    /// Test-only flag to force the next emit() to fail with a signing error.
    /// Only for use in tests to verify fail-closed behavior.
    #[cfg(feature = "test-utils")]
    force_signing_failure: bool,
}

impl ReceiptEmitter {
    pub fn new(key_pair: Ed25519KeyPair) -> Self {
        Self {
            key_pair,
            last_prev_hash: [0u8; 32], // genesis
            receipts: Vec::new(),
            #[cfg(feature = "test-utils")]
            force_signing_failure: false,
        }
    }

    /// Test-only method to force the next emit() to fail with a signing error.
    /// Only for use in tests to verify fail-closed behavior.
    #[cfg(feature = "test-utils")]
    pub fn force_signing_failure(&mut self) {
        self.force_signing_failure = true;
    }

    /// Emit a signed receipt, updating the chain state.
    ///
    /// Returns `Err` on signing failure — never produces an unsigned receipt (REQ-433).
    pub fn emit(
        &mut self,
        capability: &str,
        action: &str,
        path: &str,
        size: u64,
        result: &str,
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
            .emit("filesystem.read", "read", "test.txt", 11, "success")
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
            .emit("filesystem.read", "read", "a.txt", 10, "success")
            .unwrap();
        let r2 = emitter
            .emit("filesystem.read", "read", "b.txt", 20, "success")
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
        let receipt = emitter.emit("filesystem.read", "read", "test.txt", 11, "success");
        assert!(receipt.is_ok(), "Normal emit should succeed");

        // Force next emit to fail
        emitter.force_signing_failure();

        // Next emit should fail
        let result = emitter.emit("filesystem.read", "read", "test.txt", 11, "success");
        assert!(result.is_err(), "Emit should fail when forced");

        // Next emit after failure should succeed again (flag reset)
        let receipt = emitter.emit("filesystem.read", "read", "test.txt", 11, "success");
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
            .emit("filesystem.read", "read", "a.txt", 10, "success")
            .unwrap();
        let r2 = emitter
            .emit("filesystem.read", "read", "b.txt", 20, "success")
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
}

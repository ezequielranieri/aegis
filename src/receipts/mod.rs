//! Signed execution receipts with hash-chaining

use anyhow::Result;
use ring::signature::{self, Ed25519KeyPair};
use ring::rand::SystemRandom;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Execution receipt with cryptographic proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub module_hash: String,
    pub input_hash: String,
    pub output_hash: String,
    pub timestamp: u64,
    pub previous_receipt_hash: Option<String>,
    pub signature: String,
    pub public_key: String,
}

impl ExecutionReceipt {
    pub fn new(
        module_hash: String,
        input_hash: String,
        output_hash: String,
        previous_receipt_hash: Option<String>,
        key_pair: &Ed25519KeyPair,
    ) -> Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs();

        let payload = format!(
            "{}:{}:{}:{}:{}",
            module_hash,
            input_hash,
            output_hash,
            timestamp,
            previous_receipt_hash.as_deref().unwrap_or("")
        );

        let rng = SystemRandom::new();
        let signature = key_pair.sign(payload.as_bytes(), &rng)?;

        Ok(Self {
            module_hash,
            input_hash,
            output_hash,
            timestamp,
            previous_receipt_hash,
            signature: hex::encode(signature.as_ref()),
            public_key: hex::encode(key_pair.public_key().as_ref()),
        })
    }

    pub fn verify(&self, key_pair: &Ed25519KeyPair) -> Result<bool> {
        let payload = format!(
            "{}:{}:{}:{}:{}",
            self.module_hash,
            self.input_hash,
            self.output_hash,
            self.timestamp,
            self.previous_receipt_hash.as_deref().unwrap_or("")
        );

        let signature_bytes = hex::decode(&self.signature)?;
        let signature = signature::UnparsedPublicKey::new(&signature::ED25519, key_pair.public_key().as_ref());
        Ok(signature.verify(payload.as_bytes(), &signature_bytes).is_ok())
    }

    pub fn hash(&self) -> String {
        use ring::digest::{SHA256, digest};
        let data = serde_json::to_vec(self)?;
        hex::encode(digest(&SHA256, &data).as_ref())
    }
}

/// Receipt chain for audit trail
pub struct ReceiptChain {
    key_pair: Ed25519KeyPair,
    last_receipt_hash: Option<String>,
}

impl ReceiptChain {
    pub fn new(key_pair: Ed25519KeyPair) -> Self {
        Self {
            key_pair,
            last_receipt_hash: None,
        }
    }

    pub fn create_receipt(
        &mut self,
        module_hash: String,
        input_hash: String,
        output_hash: String,
    ) -> Result<ExecutionReceipt> {
        let receipt = ExecutionReceipt::new(
            module_hash,
            input_hash,
            output_hash,
            self.last_receipt_hash.clone(),
            &self.key_pair,
        )?;
        
        self.last_receipt_hash = Some(receipt.hash());
        Ok(receipt)
    }
}
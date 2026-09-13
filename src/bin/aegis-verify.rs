//! CLI verifier for execution receipt chains (REQ-454, S-454).
//!
//! Usage: `aegis-verify <chain.json> <public_key_b64>`
//!
//! Reads a JSON file containing an array of `ExecutionReceipt` objects,
//! verifies the hash chain integrity, Ed25519 signatures, and timestamps.

use aegis::receipts::{ExecutionReceipt, ReceiptChain};
use std::path::PathBuf;
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 3 {
        eprintln!("Usage: {} <chain.json> <public_key_b64>", args[0]);
        eprintln!();
        eprintln!("Arguments:");
        eprintln!("  chain.json       Path to JSON file containing receipt chain");
        eprintln!("  public_key_b64   Base64-encoded Ed25519 public key (32 bytes)");
        process::exit(1);
    }

    let chain_path = PathBuf::from(&args[1]);
    let public_key_b64 = &args[2];

    // Read chain file
    let chain_content = match std::fs::read_to_string(&chain_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading chain file: {}", e);
            process::exit(1);
        }
    };

    // Parse receipts
    let receipts: Vec<ExecutionReceipt> = match serde_json::from_str(&chain_content) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error parsing receipt chain: {}", e);
            process::exit(1);
        }
    };

    // Decode public key
    use base64::Engine;
    let public_key = match base64::engine::general_purpose::STANDARD.decode(public_key_b64) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Error decoding public key: {}", e);
            process::exit(1);
        }
    };

    if public_key.len() != 32 {
        eprintln!(
            "Error: public key must be 32 bytes, got {}",
            public_key.len()
        );
        process::exit(1);
    }

    // Verify chain
    println!(
        "Verifying receipt chain: {} receipts from {:?}",
        receipts.len(),
        chain_path
    );

    match ReceiptChain::verify_chain(&receipts, &public_key) {
        Ok(()) => {
            println!("OK: Receipt chain verified successfully");
            println!("  - {} receipts validated", receipts.len());
            println!("  - All signatures valid");
            println!("  - Hash chain intact");
            println!("  - All timestamps within tolerance");
            process::exit(0);
        }
        Err(e) => {
            eprintln!("FAILED: {}", e);
            process::exit(1);
        }
    }
}

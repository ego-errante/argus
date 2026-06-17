//! Loads the signing Keypair from the gitignored secrets/ file (ADR 0002).

use anyhow::{anyhow, Result};
use solana_sdk::signer::keypair::Keypair;

/// Load the fee-payer Keypair from a Solana CLI keypair JSON file
/// (a 64-byte array: 32-byte ed25519 seed + 32-byte pubkey).
pub fn load_keypair(path: &str) -> Result<Keypair> {
    let data =
        std::fs::read_to_string(path).map_err(|e| anyhow!("reading keypair {path}: {e}"))?;
    let bytes: Vec<u8> =
        serde_json::from_str(&data).map_err(|e| anyhow!("parsing keypair JSON {path}: {e}"))?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow!("invalid keypair bytes in {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signer::Signer;

    #[test]
    fn loads_keypair_roundtrip() {
        let kp = Keypair::new();
        let json = serde_json::to_string(&kp.to_bytes().to_vec()).unwrap();
        let path = std::env::temp_dir().join(format!("argus-test-kp-{}.json", std::process::id()));
        std::fs::write(&path, json).unwrap();

        let loaded = load_keypair(path.to_str().unwrap()).expect("load");
        assert_eq!(loaded.pubkey(), kp.pubkey(), "loaded keypair must match the written one");

        std::fs::remove_file(&path).ok();
    }
}

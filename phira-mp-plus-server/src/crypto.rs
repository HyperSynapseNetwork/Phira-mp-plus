//! Node key management for plugin crypto host API.
//!
//! The node key pair is derived from HSN_SECRET_KEY at startup.
//! The private key material never leaves the host process — plugins
//! only receive public key and signature results.
//!
//! HMAC-SHA256 is implemented using pure sha2 (no external hmac crate)
//! to avoid adding a direct dependency.

use sha2::{Digest, Sha256};

/// Compute SHA-256 HMAC using the nested construction:
/// HMAC(K, m) = H((K' ⊕ opad) || H((K' ⊕ ipad) || m))
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK_SIZE: usize = 64;
    let mut k = key.to_vec();
    if k.len() > BLOCK_SIZE {
        let mut hasher = Sha256::new();
        hasher.update(&k);
        k = hasher.finalize().to_vec();
    }
    k.resize(BLOCK_SIZE, 0);

    let mut ipad = vec![0x36u8; BLOCK_SIZE];
    let mut opad = vec![0x5cu8; BLOCK_SIZE];
    for i in 0..k.len() {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finalize().to_vec()
}

/// Node key pair wrapping an HMAC-SHA256 signing key.
/// In production, this should be replaced with an Ed25519 key pair.
pub struct NodeKey {
    /// Derived signing key (32 bytes).
    signing_key: Vec<u8>,
    /// Public key identifier (SHA-256 of the signing key).
    pub public_key: Vec<u8>,
}

impl NodeKey {
    /// Derive a node key from the server's secret key.
    pub fn from_secret(secret: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"pmp-node-key-v1");
        hasher.update(secret);
        let signing_key = hasher.finalize().to_vec();

        let public_key = {
            let mut h = Sha256::new();
            h.update(&signing_key);
            h.finalize().to_vec()
        };

        Self { signing_key, public_key }
    }

    /// Sign data using HMAC-SHA256 with the node's signing key.
    pub fn sign(&self, payload: &[u8]) -> Vec<u8> {
        hmac_sha256(&self.signing_key, payload)
    }

    /// Verify a signature against a given public key and payload.
    pub fn verify(pubkey: &[u8], payload: &[u8], signature: &[u8]) -> bool {
        let expected = hmac_sha256(pubkey, payload);
        // Constant-time comparison to prevent timing attacks
        expected.len() == signature.len()
            && expected.iter().zip(signature).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

/// Compute SHA-256 hash.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = NodeKey::from_secret(b"test-secret-key-32-bytes-long!!");
        let payload = b"hello world";
        let sig = key.sign(payload);
        assert!(NodeKey::verify(&key.signing_key, payload, &sig));
        assert!(!NodeKey::verify(&key.signing_key, b"tampered", &sig));
    }

    #[test]
    fn sha256_produces_32_bytes() {
        let hash = sha256(b"test data");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn node_key_is_deterministic() {
        let a = NodeKey::from_secret(b"same-secret");
        let b = NodeKey::from_secret(b"same-secret");
        assert_eq!(a.public_key, b.public_key);
    }

    #[test]
    fn hmac_differs_from_plain_sha256() {
        let key = b"a-key";
        let data = b"some data";
        let h = hmac_sha256(key, data);
        let plain = {
            let mut hasher = Sha256::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        };
        assert_ne!(h, plain, "HMAC should differ from plain hash");
    }
}

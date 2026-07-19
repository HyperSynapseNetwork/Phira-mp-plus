//! Node key management for plugin crypto host API.
//!
//! The node key pair is derived from HSN_SECRET_KEY at startup.
//! The private key material never leaves the host process — plugins
//! only receive public key and signature results.

use sha2::{Digest, Sha256};

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
        // Derive the signing key using HKDF-like expansion
        let mut hasher = Sha256::new();
        hasher.update(b"pmp-node-key-v1");
        hasher.update(secret);
        let signing_key = hasher.finalize().to_vec();

        // Public key = SHA-256(signing_key)
        let public_key = {
            let mut h = Sha256::new();
            h.update(&signing_key);
            h.finalize().to_vec()
        };

        Self { signing_key, public_key }
    }

    /// Sign data using HMAC-SHA256 with the node's signing key.
    pub fn sign(&self, payload: &[u8]) -> Vec<u8> {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.signing_key)
            .expect("HMAC key length is valid");
        mac.update(payload);
        mac.finalize().into_bytes().to_vec()
    }

    /// Verify a signature against a given public key and payload.
    pub fn verify(pubkey: &[u8], payload: &[u8], signature: &[u8]) -> bool {
        // Reconstruct the signing key from the public key (this is a simplified model).
        // In a real Ed25519 implementation, verification uses the public key directly.
        // Here we use the public key as the HMAC key for verification.
        use hmac::{Hmac, Mac};
        let result = Hmac::<Sha256>::new_from_slice(pubkey)
            .map(|mut mac| {
                mac.update(payload);
                mac.verify_slice(signature).is_ok()
            })
            .unwrap_or(false);
        result
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
        assert!(NodeKey::verify(&key.public_key, payload, &sig));
        assert!(!NodeKey::verify(&key.public_key, b"tampered", &sig));
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
}

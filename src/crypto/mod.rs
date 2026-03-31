use anyhow::Result;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::{SecureRandom, SystemRandom};

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Handles X25519 key exchange and AES-256-GCM encryption for the data channel.
pub struct CryptoContext {
    key: LessSafeKey,
    rng: SystemRandom,
    /// Counter-based nonce to avoid reuse (monotonically increasing)
    nonce_counter: u64,
}

/// Key pair for X25519 key exchange
pub struct KeyPair {
    private_key: EphemeralPrivateKey,
    pub public_key_bytes: Vec<u8>,
}

impl KeyPair {
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let private_key = EphemeralPrivateKey::generate(&X25519, &rng)
            .map_err(|e| anyhow::anyhow!("key generation failed: {}", e))?;
        let public_key_bytes = private_key
            .compute_public_key()
            .map_err(|e| anyhow::anyhow!("public key computation failed: {}", e))?
            .as_ref()
            .to_vec();

        Ok(Self {
            private_key,
            public_key_bytes,
        })
    }

    /// Perform X25519 key agreement and derive an AES-256-GCM key
    pub fn agree(self, peer_public_key: &[u8]) -> Result<CryptoContext> {
        let peer_key = UnparsedPublicKey::new(&X25519, peer_public_key);

        let shared_secret: Vec<u8> = agreement::agree_ephemeral(
            self.private_key,
            &peer_key,
            |secret| {
                // Derive AES-256 key from shared secret using HKDF
                let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, b"updown-v1");
                let prk = salt.extract(secret);
                let info = [b"aes-256-gcm-key" as &[u8]];
                let okm = prk.expand(&info, &AES_256_GCM)?;
                let mut key_bytes = vec![0u8; 32];
                okm.fill(&mut key_bytes)?;
                Ok(key_bytes)
            },
        )
        .map_err(|_| anyhow::anyhow!("key agreement failed"))?
        .map_err(|_: ring::error::Unspecified| anyhow::anyhow!("HKDF key derivation failed"))?;

        let unbound_key = UnboundKey::new(&AES_256_GCM, &shared_secret)
            .map_err(|e| anyhow::anyhow!("key creation failed: {}", e))?;

        Ok(CryptoContext {
            key: LessSafeKey::new(unbound_key),
            rng: SystemRandom::new(),
            nonce_counter: 0,
        })
    }
}

impl CryptoContext {
    /// Create a CryptoContext directly from a shared key (for testing)
    pub fn from_key(key_bytes: &[u8; 32]) -> Result<Self> {
        let unbound_key = UnboundKey::new(&AES_256_GCM, key_bytes)
            .map_err(|e| anyhow::anyhow!("key creation failed: {}", e))?;
        Ok(Self {
            key: LessSafeKey::new(unbound_key),
            rng: SystemRandom::new(),
            nonce_counter: 0,
        })
    }

    /// Encrypt data in-place, appending the auth tag.
    /// Returns the nonce used (must be sent with the ciphertext).
    pub fn encrypt(&mut self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let nonce_bytes = self.next_nonce();
        let nonce =
            Nonce::try_assume_unique_for_key(&nonce_bytes).map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut buf = Vec::with_capacity(NONCE_LEN + plaintext.len() + TAG_LEN);
        buf.extend_from_slice(&nonce_bytes);
        buf.extend_from_slice(plaintext);

        self.key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut buf[NONCE_LEN..])
            .map(|tag| {
                buf.extend_from_slice(tag.as_ref());
                buf
            })
            .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))
    }

    /// Encrypt into a pre-allocated buffer. Writes [nonce][ciphertext][tag].
    /// Returns the number of bytes written. The buffer must have capacity for
    /// NONCE_LEN + plaintext.len() + TAG_LEN bytes starting at `offset`.
    pub fn encrypt_into(
        &mut self,
        plaintext: &[u8],
        aad: &[u8],
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        let nonce_bytes = self.next_nonce();
        let nonce =
            Nonce::try_assume_unique_for_key(&nonce_bytes).map_err(|e| anyhow::anyhow!("{}", e))?;

        let start = buf.len();
        buf.extend_from_slice(&nonce_bytes);
        buf.extend_from_slice(plaintext);

        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut buf[start + NONCE_LEN..])
            .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;
        buf.extend_from_slice(tag.as_ref());
        Ok(())
    }

    /// Decrypt data. Input format: [nonce (12 bytes)][ciphertext][tag (16 bytes)]
    pub fn decrypt(&self, ciphertext_with_nonce: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        if ciphertext_with_nonce.len() < NONCE_LEN + TAG_LEN {
            anyhow::bail!("ciphertext too short");
        }

        let (nonce_bytes, ciphertext_and_tag) = ciphertext_with_nonce.split_at(NONCE_LEN);
        let nonce =
            Nonce::try_assume_unique_for_key(nonce_bytes).map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut buf = ciphertext_and_tag.to_vec();
        let plaintext = self
            .key
            .open_in_place(nonce, Aad::from(aad), &mut buf)
            .map_err(|e| anyhow::anyhow!("decryption failed: {}", e))?;

        Ok(plaintext.to_vec())
    }

    fn next_nonce(&mut self) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        // Use counter in first 8 bytes, random in last 4 for extra safety
        nonce[..8].copy_from_slice(&self.nonce_counter.to_le_bytes());
        let _ = self.rng.fill(&mut nonce[8..]);
        self.nonce_counter += 1;
        nonce
    }
}

/// Total overhead added by encryption per packet (nonce + tag)
pub const ENCRYPTION_OVERHEAD: usize = NONCE_LEN + TAG_LEN;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let mut ctx_enc = CryptoContext::from_key(&key).unwrap();
        let ctx_dec = CryptoContext::from_key(&key).unwrap();

        let plaintext = b"hello, fast transfer world!";
        let aad = b"block-0";

        let ciphertext = ctx_enc.encrypt(plaintext, aad).unwrap();
        assert_ne!(&ciphertext[NONCE_LEN..], plaintext);

        let decrypted = ctx_dec.decrypt(&ciphertext, aad).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let key = [42u8; 32];
        let mut ctx_enc = CryptoContext::from_key(&key).unwrap();
        let ctx_dec = CryptoContext::from_key(&key).unwrap();

        let plaintext = b"secret data";
        let aad = b"block-1";

        let mut ciphertext = ctx_enc.encrypt(plaintext, aad).unwrap();
        // Flip a bit in the ciphertext
        ciphertext[NONCE_LEN + 3] ^= 0xFF;

        assert!(ctx_dec.decrypt(&ciphertext, aad).is_err());
    }

    #[test]
    fn test_wrong_aad_fails() {
        let key = [42u8; 32];
        let mut ctx_enc = CryptoContext::from_key(&key).unwrap();
        let ctx_dec = CryptoContext::from_key(&key).unwrap();

        let plaintext = b"secret data";
        let ciphertext = ctx_enc.encrypt(plaintext, b"correct-aad").unwrap();

        assert!(ctx_dec.decrypt(&ciphertext, b"wrong-aad").is_err());
    }
}

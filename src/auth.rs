use anyhow::{anyhow, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn secret_proof(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("locho-v1:{secret}").as_bytes());
    hex::encode(hasher.finalize())
}

pub fn verify_secret_proof(secret: &str, proof: &str) -> Result<()> {
    if secret_proof(secret) == proof {
        Ok(())
    } else {
        Err(anyhow!("invalid secret proof"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_random_hex_and_verifies() {
        let secret = generate_secret();
        assert_eq!(secret.len(), 64);
        assert!(verify_secret_proof(&secret, &secret_proof(&secret)).is_ok());
        assert!(verify_secret_proof("wrong", &secret_proof(&secret)).is_err());
    }
}

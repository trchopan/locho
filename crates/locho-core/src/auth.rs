use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn secret_proof(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("locho-v1:{secret}").as_bytes());
    hex::encode(hasher.finalize())
}

pub fn verify_secret_proof(secret: &str, proof: &str) -> Result<()> {
    let expected = secret_proof(secret);
    if expected.as_bytes().ct_eq(proof.as_bytes()).into() {
        Ok(())
    } else {
        Err(anyhow!("invalid secret proof"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_matches_existing_protocol_vector() {
        let proof = "eabe5829e4d19af26dfdb3bf739b35d02b461bbb38b19fe279daffb02b42041d";
        assert_eq!(secret_proof("test-secret"), proof);
        assert!(verify_secret_proof("test-secret", proof).is_ok());
    }

    #[test]
    fn proof_verifies_only_for_matching_secret() {
        let secret = "test-secret";
        assert!(verify_secret_proof(secret, &secret_proof(secret)).is_ok());
        assert!(verify_secret_proof("wrong", &secret_proof(secret)).is_err());
    }

    #[test]
    fn malformed_and_modified_proofs_are_rejected() {
        let secret = "test-secret";
        let proof = secret_proof(secret);
        let replacement = if &proof[..1] == "0" { "1" } else { "0" };
        let modified = format!("{replacement}{}", &proof[1..]);

        assert!(verify_secret_proof(secret, "not-a-proof").is_err());
        assert!(verify_secret_proof(secret, &modified).is_err());
    }
}

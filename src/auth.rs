pub use locho_core::auth::{secret_proof, verify_secret_proof};

pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_random_hex_and_verifies() {
        let secret = generate_secret();
        assert_eq!(secret.len(), 64);
        assert!(verify_secret_proof(&secret, &secret_proof(&secret)).is_ok());
        assert_ne!(secret, generate_secret());
    }
}

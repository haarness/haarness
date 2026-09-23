//! PKCE S256 helpers (RFC 7636; DESIGN.md §5.2 "Native PKCE").

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};

/// `code_verifier` length in random bytes; 32 bytes → 43 base64url chars (the RFC minimum).
pub const VERIFIER_BYTES: usize = 32;
/// `code_challenge_method` sent to `/oauth/authorize`.
pub const CHALLENGE_METHOD: &str = "S256";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkce {
    /// 43–128 unreserved chars.
    pub verifier: String,
    /// `base64url(sha256(verifier))` without padding.
    pub challenge: String,
}

impl Pkce {
    /// A fresh random verifier and its challenge.
    pub fn generate() -> Self {
        let verifier = random_urlsafe(VERIFIER_BYTES);
        let challenge = challenge_for(&verifier);
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// `base64url(sha256(verifier))`, no padding.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `n` random bytes as unpadded base64url (used for the verifier and the OAuth `state`).
pub fn random_urlsafe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_within_rfc_bounds_and_verifies() {
        let pkce = Pkce::generate();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert_eq!(challenge_for(&pkce.verifier), pkce.challenge);
    }
}

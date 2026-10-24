//! How SwitchBot's API v1.1 knows a request is the account's: an HMAC-SHA256 of the token, the
//! time and a nonce, keyed with the secret, in base64.
//!
//! SwitchBot's README says to upper-case the signature and leaves the nonce out of its formula;
//! its own sample code does neither, and the sample code is what the server checks. This follows
//! the code.

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::settings::Account;

/// The headers that sign one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// Milliseconds since 1970.
    pub t: String,
    pub nonce: String,
    pub sign: String,
}

pub fn sign(account: &Account, t_millis: u128, nonce: &str) -> Signed {
    let t = t_millis.to_string();
    let message = format!("{}{t}{nonce}", account.token.expose());
    let mac = hmac_sha256(account.secret.expose().as_bytes(), message.as_bytes());
    Signed {
        t,
        nonce: nonce.to_owned(),
        sign: base64::engine::general_purpose::STANDARD.encode(mac),
    }
}

/// A fresh nonce: 16 random bytes as hex.
pub fn nonce() -> String {
    let mut bytes = [0u8; 16];
    // The OS random source failing leaves zeros; the timestamp still changes every request.
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// HMAC (RFC 2104) over SHA-256.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|k| k ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_cloud::Secret;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than a block is hashed first.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_request_is_signed_the_way_switchbots_sample_code_signs_it() {
        let account = Account {
            token: Secret::from("token".to_owned()),
            secret: Secret::from("secret".to_owned()),
        };
        let signed = sign(&account, 1_700_000_000_000, "nonce");
        assert_eq!(signed.t, "1700000000000");
        // base64(HMAC-SHA256("secret", "token1700000000000nonce")), as Python's
        // `base64.b64encode(hmac.new(secret, msg, hashlib.sha256).digest())` gives it.
        assert_eq!(signed.sign, "Ho/pm1Q6hyf9kroxzCu/cSBo7lGKad4tesq6eb2CpUg=");
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
        assert_eq!(nonce().len(), 32);
    }
}

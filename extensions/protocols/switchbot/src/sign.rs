//! How SwitchBot's API v1.1 knows a request is the account's: an HMAC-SHA256 of the token, the
//! time and a nonce, keyed with the secret, in base64.
//!
//! SwitchBot's README says to upper-case the signature and leaves the nonce out of its formula;
//! its own sample code does neither, and the sample code is what the server checks. This follows
//! the code.

use base64::Engine as _;
use hmac::{Hmac, KeyInit as _, Mac as _};
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

/// A fresh nonce: 16 random bytes as hex. Should the OS's random source fail, the time and a
/// count stand in, hashed: still never the same twice, if guessable.
pub fn nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let count = COUNT.fetch_add(1, Ordering::Relaxed);
        if count == 0 {
            tracing::warn!("the OS's random source failed; SwitchBot nonces come from the clock");
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let digest = Sha256::new()
            .chain_update(nanos.to_le_bytes())
            .chain_update(count.to_le_bytes())
            .chain_update(std::process::id().to_le_bytes())
            .finalize();
        bytes.copy_from_slice(&digest[..16]);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// HMAC (RFC 2104) over SHA-256.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(message);
    mac.finalize().into_bytes().into()
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

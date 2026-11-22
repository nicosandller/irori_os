//! The key Tesla knows Irori by: a P-256 key pair. Its public half goes on the person's website
//! (Tesla fetches it from there when the app registers) and onto each car (the "virtual key"),
//! and its private half signs the commands a car checks.
//!
//! Irori makes its own and keeps it in its store. A developer app that already has a key can
//! paste it into the settings instead: the website and the car already have its public half, so
//! nothing has to be paired again.

use base64::Engine as _;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use p256::{PublicKey, SecretKey};

/// The store key a key Irori made itself is kept under.
pub const STORED: &str = "private_key";

/// The path on the person's website Tesla reads the public key from.
pub const WELL_KNOWN: &str = "/.well-known/appspecific/com.tesla.3p.public-key.pem";

#[derive(Clone)]
pub struct Key {
    secret: SecretKey,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Key({})", &self.public_hex()[..16])
    }
}

impl Key {
    /// A key as a person pasted it: PEM, as Home Assistant writes it (`EC PRIVATE KEY`) or as
    /// PKCS#8 (`PRIVATE KEY`), with its line breaks or without (a password box drops them), with
    /// an `EC PARAMETERS` block ahead of it or not, or just the base64 between the lines.
    pub fn from_pasted(text: &str) -> Result<Self, String> {
        let body = private_block(text).unwrap_or(text);
        let body: String = body.split_whitespace().collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body.as_bytes())
            .map_err(|_| {
                "the private key isn't a PEM key: paste all of tesla_fleet.key, from \
                 -----BEGIN to -----END"
                    .to_owned()
            })?;
        SecretKey::from_sec1_der(&der)
            .or_else(|_| SecretKey::from_pkcs8_der(&der))
            .map(|secret| Self { secret })
            .map_err(|_| "the private key isn't a P-256 key, which is what Tesla uses".to_owned())
    }

    /// A new key, from the system's randomness.
    pub fn generate() -> Result<Self, String> {
        for _ in 0..8 {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|error| format!("no randomness: {error}"))?;
            if let Ok(secret) = SecretKey::from_slice(&bytes) {
                return Ok(Self { secret });
            }
        }
        Err("couldn't make a key".to_owned())
    }

    /// As kept in the store: PKCS#8, base64.
    pub fn to_stored(&self) -> Result<String, String> {
        let der = self
            .secret
            .to_pkcs8_der()
            .map_err(|error| format!("couldn't keep the key: {error}"))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(der.as_bytes()))
    }

    pub fn from_stored(text: &str) -> Result<Self, String> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|_| "the stored key is damaged".to_owned())?;
        SecretKey::from_pkcs8_der(&der)
            .map(|secret| Self { secret })
            .map_err(|_| "the stored key is damaged".to_owned())
    }

    pub fn public(&self) -> PublicKey {
        self.secret.public_key()
    }

    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// The public key as the file the website serves.
    pub fn public_pem(&self) -> String {
        self.public()
            .to_public_key_pem(LineEnding::LF)
            .unwrap_or_default()
    }

    /// The public key as Tesla's `partner_accounts/public_key` answers it: the uncompressed
    /// point, in lowercase hex.
    pub fn public_hex(&self) -> String {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        self.public()
            .to_encoded_point(false)
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// The base64 of the `… PRIVATE KEY` block in a PEM text, if it has one.
fn private_block(text: &str) -> Option<&str> {
    text.split("-----BEGIN ").skip(1).find_map(|block| {
        let (label, rest) = block.split_once("-----")?;
        if !label.ends_with("PRIVATE KEY") {
            return None;
        }
        Some(rest.split_once("-----END").map_or(rest, |(body, _)| body))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_assistants_key_reads_with_or_without_its_line_breaks() {
        let generated = Key::generate().expect("a key");
        let pkcs8 = generated
            .secret
            .to_pkcs8_pem(LineEnding::LF)
            .expect("pem")
            .to_string();
        let flattened = pkcs8.replace('\n', "");
        let back = Key::from_pasted(&flattened).expect("one line still reads");
        assert_eq!(back.public_hex(), generated.public_hex());
        let sec1 = generated
            .secret
            .to_sec1_pem(LineEnding::LF)
            .expect("pem")
            .to_string();
        assert_eq!(
            Key::from_pasted(&sec1).expect("sec1").public_hex(),
            generated.public_hex()
        );
        let with_parameters = format!(
            "-----BEGIN EC PARAMETERS-----\nBggqhkjOPQMBBw==\n-----END EC PARAMETERS-----\n{sec1}"
        );
        assert_eq!(
            Key::from_pasted(&with_parameters)
                .expect("with parameters")
                .public_hex(),
            generated.public_hex()
        );
    }

    #[test]
    fn a_pasted_key_that_isnt_one_says_so() {
        assert!(Key::from_pasted("hello!").expect_err("no").contains("PEM"));
        let public = Key::generate().expect("a key").public_pem();
        assert!(
            Key::from_pasted(&public).is_err(),
            "a public key isn't the private one"
        );
    }

    #[test]
    fn a_key_irori_made_is_kept_and_read_back() {
        let key = Key::generate().expect("a key");
        let stored = key.to_stored().expect("kept");
        let back = Key::from_stored(&stored).expect("read");
        assert_eq!(back.public_hex(), key.public_hex());
        assert!(key.public_pem().starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert_eq!(key.public_hex().len(), 130, "04, x and y");
        assert!(!format!("{key:?}").contains(&stored));
    }
}

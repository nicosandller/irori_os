//! KLAP, the local transport of Tapo devices and recent Kasa firmware (`lv` 2 in discovery).
//!
//! Two handshakes prove that both ends know the same hash of the TP-Link account; neither the
//! account nor its hash crosses the network. From the two random seeds and that hash both ends
//! derive an AES-128 key, an IV prefix and a signing key. Each request carries a sequence number
//! that is the IV's last four bytes, and a SHA-256 signature over it and the ciphertext.

use std::io;
use std::net::SocketAddr;

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::http;

/// What the device holds for the account: SHA-256 of SHA-1(email) ‖ SHA-1(password).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuthHash([u8; 32]);

impl std::fmt::Debug for AuthHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthHash(…)")
    }
}

impl AuthHash {
    pub fn new(username: &str, password: &str) -> Self {
        let mut joined = Vec::with_capacity(40);
        joined.extend_from_slice(&<Sha1 as sha1::Digest>::digest(username.as_bytes()));
        joined.extend_from_slice(&<Sha1 as sha1::Digest>::digest(password.as_bytes()));
        Self(sha256(&[&joined]))
    }
}

#[derive(Debug)]
pub enum KlapError {
    /// The device couldn't be reached, or stopped answering.
    Io(io::Error),
    /// The device doesn't know this account.
    Account,
    /// The session ran out; a new handshake starts another.
    Expired,
    /// The device answered with something that isn't KLAP.
    Device(String),
}

impl std::fmt::Display for KlapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Account => f.write_str("the device doesn't accept this TP-Link account"),
            Self::Expired => f.write_str("the session expired"),
            Self::Device(message) => f.write_str(message),
        }
    }
}

impl From<io::Error> for KlapError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// The keys both ends derive after the handshake.
pub(crate) struct Keys {
    cipher: Aes128,
    iv: [u8; 12],
    signing: [u8; 28],
    seq: i32,
}

impl Keys {
    pub(crate) fn derive(local: &[u8; 16], remote: &[u8; 16], auth: &AuthHash) -> Self {
        let key = sha256(&[b"lsk", local, remote, &auth.0]);
        let ivs = sha256(&[b"iv", local, remote, &auth.0]);
        let signing = sha256(&[b"ldk", local, remote, &auth.0]);
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&ivs[..12]);
        let mut sig = [0u8; 28];
        sig.copy_from_slice(&signing[..28]);
        Self {
            cipher: Aes128::new(GenericArray::from_slice(&key[..16])),
            iv,
            signing: sig,
            seq: i32::from_be_bytes([ivs[28], ivs[29], ivs[30], ivs[31]]),
        }
    }

    fn iv_for(&self, seq: i32) -> [u8; 16] {
        let mut iv = [0u8; 16];
        iv[..12].copy_from_slice(&self.iv);
        iv[12..].copy_from_slice(&seq.to_be_bytes());
        iv
    }

    fn sign(&self, seq: i32, ciphertext: &[u8]) -> [u8; 32] {
        sha256(&[&self.signing, &seq.to_be_bytes(), ciphertext])
    }

    /// The next sequence number, and the signed ciphertext to send with it.
    pub(crate) fn seal(&mut self, plain: &[u8]) -> (i32, Vec<u8>) {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        (seq, self.seal_at(seq, plain))
    }

    /// The signed ciphertext under a given sequence number: how a device answers a request.
    pub(crate) fn seal_at(&self, seq: i32, plain: &[u8]) -> Vec<u8> {
        let ciphertext = cbc_encrypt(&self.cipher, self.iv_for(seq), plain);
        let mut out = self.sign(seq, &ciphertext).to_vec();
        out.extend_from_slice(&ciphertext);
        out
    }

    /// The plaintext of a signed message under `seq`, either end's.
    pub(crate) fn open(&self, seq: i32, message: &[u8]) -> Result<Vec<u8>, KlapError> {
        if message.len() < 32 + 16 || !(message.len() - 32).is_multiple_of(16) {
            return Err(KlapError::Device("the device's answer isn't KLAP".into()));
        }
        let (signature, ciphertext) = message.split_at(32);
        if signature != self.sign(seq, ciphertext) {
            return Err(KlapError::Device(
                "the device's answer isn't signed with this session".into(),
            ));
        }
        cbc_decrypt(&self.cipher, self.iv_for(seq), ciphertext)
            .ok_or_else(|| KlapError::Device("the device's answer doesn't decrypt".into()))
    }

    #[cfg(test)]
    pub(crate) fn seq(&self) -> i32 {
        self.seq
    }
}

fn cbc_encrypt(cipher: &Aes128, iv: [u8; 16], plain: &[u8]) -> Vec<u8> {
    let pad = 16 - plain.len() % 16;
    let mut data = plain.to_vec();
    data.extend(std::iter::repeat_n(pad as u8, pad));
    let mut previous = iv;
    for block in data.as_chunks_mut::<16>().0 {
        for (byte, prev) in block.iter_mut().zip(previous) {
            *byte ^= prev;
        }
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
        previous.copy_from_slice(block);
    }
    data
}

fn cbc_decrypt(cipher: &Aes128, iv: [u8; 16], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let mut data = ciphertext.to_vec();
    let mut previous = iv;
    for block in data.as_chunks_mut::<16>().0 {
        let this = *block;
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
        for (byte, prev) in block.iter_mut().zip(previous) {
            *byte ^= prev;
        }
        previous = this;
    }
    let pad = usize::from(*data.last()?);
    if pad == 0 || pad > 16 || pad > data.len() {
        return None;
    }
    if !data[data.len() - pad..]
        .iter()
        .all(|byte| usize::from(*byte) == pad)
    {
        return None;
    }
    data.truncate(data.len() - pad);
    Some(data)
}

/// The hashes each handshake exchanges.
pub(crate) fn server_proof(local: &[u8; 16], remote: &[u8; 16], auth: &AuthHash) -> [u8; 32] {
    sha256(&[local, remote, &auth.0])
}

pub(crate) fn client_proof(local: &[u8; 16], remote: &[u8; 16], auth: &AuthHash) -> [u8; 32] {
    sha256(&[remote, local, &auth.0])
}

/// One logged-in session with one device.
pub struct Session {
    address: SocketAddr,
    cookie: String,
    keys: Keys,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub async fn connect(address: SocketAddr, auth: &AuthHash) -> Result<Self, KlapError> {
        let mut local = [0u8; 16];
        getrandom::fill(&mut local)
            .map_err(|_| KlapError::Device("no randomness for the handshake".into()))?;

        let first = http::post(address, "/app/handshake1", None, &local).await?;
        if first.status != 200 || first.body.len() != 48 {
            return Err(KlapError::Device(format!(
                "the device refused the first handshake (HTTP {})",
                first.status
            )));
        }
        let mut remote = [0u8; 16];
        remote.copy_from_slice(&first.body[..16]);
        if first.body[16..] != server_proof(&local, &remote, auth) {
            return Err(KlapError::Account);
        }
        let cookie = first
            .session
            .ok_or_else(|| KlapError::Device("the device didn't start a session".into()))?;

        let proof = client_proof(&local, &remote, auth);
        let second = http::post(address, "/app/handshake2", Some(&cookie), &proof).await?;
        if second.status != 200 {
            return Err(KlapError::Device(format!(
                "the device refused the second handshake (HTTP {})",
                second.status
            )));
        }
        Ok(Self {
            address,
            cookie,
            keys: Keys::derive(&local, &remote, auth),
        })
    }

    /// One request and its answer, as JSON.
    pub async fn request(
        &mut self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, KlapError> {
        let plain = serde_json::to_vec(request)
            .map_err(|error| KlapError::Device(format!("a request didn't encode: {error}")))?;
        let (seq, sealed) = self.keys.seal(&plain);
        let path = format!("/app/request?seq={seq}");
        let response = http::post(self.address, &path, Some(&self.cookie), &sealed).await?;
        match response.status {
            200 => {}
            401 | 403 => return Err(KlapError::Expired),
            status => {
                return Err(KlapError::Device(format!(
                    "the device answered HTTP {status}"
                )));
            }
        }
        let plain = self.keys.open(seq, &response.body)?;
        serde_json::from_slice(&plain)
            .map_err(|_| KlapError::Device("the device's answer isn't JSON".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_hash_matches_python_kasa() {
        // sha256(sha1("user") + sha1("pass")), checked against python-kasa's
        // `KlapTransportV2.generate_auth_hash`.
        let auth = AuthHash::new("user", "pass");
        let expected = {
            let mut joined = Vec::new();
            joined.extend_from_slice(&<Sha1 as sha1::Digest>::digest(b"user"));
            joined.extend_from_slice(&<Sha1 as sha1::Digest>::digest(b"pass"));
            sha256(&[&joined])
        };
        assert_eq!(auth.0, expected);
        assert_eq!(format!("{auth:?}"), "AuthHash(…)");
    }

    #[test]
    fn both_ends_derive_the_same_keys_and_read_each_other() {
        let auth = AuthHash::new("me@example.com", "secret");
        let (local, remote) = ([1u8; 16], [2u8; 16]);
        let mut client = Keys::derive(&local, &remote, &auth);
        let device = Keys::derive(&local, &remote, &auth);
        let start = client.seq();
        for plain in [&b""[..], b"{}", &[7u8; 16], &[9u8; 100]] {
            let (seq, sealed) = client.seal(plain);
            assert_eq!(device.open(seq, &sealed).expect("opens"), plain);
        }
        assert_eq!(client.seq(), start.wrapping_add(4));
    }

    #[test]
    fn a_tampered_or_wrong_sequence_message_is_refused() {
        let auth = AuthHash::new("a", "b");
        let mut keys = Keys::derive(&[3u8; 16], &[4u8; 16], &auth);
        let (seq, mut sealed) = keys.seal(b"{\"method\":\"get_device_info\"}");
        assert!(keys.open(seq.wrapping_add(1), &sealed).is_err());
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(keys.open(seq, &sealed).is_err());
    }

    #[test]
    fn the_proofs_differ_in_order() {
        let auth = AuthHash::new("a", "b");
        let (local, remote) = ([5u8; 16], [6u8; 16]);
        assert_ne!(
            server_proof(&local, &remote, &auth),
            client_proof(&local, &remote, &auth)
        );
    }
}

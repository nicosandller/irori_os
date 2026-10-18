//! An encrypted backup (`docs/specs/backup.md` §2).
//!
//! ```text
//! "IRORIBK" 0x01 | u32 header length (big endian) | header JSON | chunk | chunk | …
//! ```
//!
//! The header is public: the format, the key derivation (argon2id, with its parameters and
//! salt), the stream's nonce, and what a listing needs to show (version, time, extensions).
//! The body is the gzipped tar, cut into 64 KiB chunks and sealed with XChaCha20-Poly1305 in
//! the STREAM construction, so a chunk can't be moved, dropped or repeated, and the last one is
//! marked, so a file cut short is caught rather than read as a smaller backup. The whole header
//! is the associated data of every chunk, so it can't be edited either.
//!
//! A wrong key and a damaged file look the same to the cipher: the first chunk doesn't open.

use std::io::{self, Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::Payload;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::{Key as CipherKey, KeyInit as _, XChaCha20Poly1305};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"IRORIBK\x01";
/// Plaintext bytes per sealed chunk.
const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
/// XChaCha20's 24-byte nonce, less the 5 bytes STREAM uses for its counter and last-block flag.
const NONCE: usize = 19;
const MAX_HEADER: usize = 64 * 1024;
/// What the recovery key is made of: Crockford's base32, which has no I, L, O or U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub cipher: String,
    pub kdf: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
    pub nonce: String,
    /// What a listing shows without the key.
    pub meta: super::Manifest,
}

/// A recovery key: 25 characters in groups of five, 125 bits.
pub fn generate_key() -> String {
    let mut bytes = [0_u8; 25];
    getrandom::fill(&mut bytes).expect("the system has randomness");
    let letters: Vec<char> = bytes
        .iter()
        .map(|b| ALPHABET[usize::from(b & 31)] as char)
        .collect();
    letters
        .chunks(5)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// What the person typed, as the key it stands for. A recovery key is read without its dashes
/// or its case, and with the look-alike letters Crockford allows; anything else is a passphrase
/// of their own and is used exactly as typed.
pub fn normalize_key(typed: &str) -> String {
    let squeezed: String = typed
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .collect::<String>()
        .to_ascii_uppercase()
        .replace('O', "0")
        .replace(['I', 'L'], "1");
    if squeezed.len() == 25 && squeezed.bytes().all(|b| ALPHABET.contains(&b)) {
        squeezed
            .as_bytes()
            .chunks(5)
            .map(|g| String::from_utf8_lossy(g).into_owned())
            .collect::<Vec<_>>()
            .join("-")
    } else {
        typed.to_owned()
    }
}

/// Whether the file at `head` starts like an encrypted backup.
pub fn is_encrypted(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

fn derive(key: &str, header: &Header) -> Result<[u8; 32], String> {
    let salt = B64
        .decode(&header.salt)
        .map_err(|_| "the header's salt is damaged")?;
    let params = Params::new(header.m_cost, header.t_cost, header.p_cost, Some(32))
        .map_err(|e| format!("the header's key settings are refused: {e}"))?;
    let mut out = [0_u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(normalize_key(key).as_bytes(), &salt, &mut out)
        .map_err(|e| format!("couldn't derive the key: {e}"))?;
    Ok(out)
}

/// Starts an encrypted stream into `out`: writes the header, and returns the writer for the
/// body. Call [`EncryptWriter::finish`] to close it.
pub fn encrypt_to<W: Write>(
    mut out: W,
    key: &str,
    meta: super::Manifest,
) -> io::Result<EncryptWriter<W>> {
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; NONCE];
    getrandom::fill(&mut salt).map_err(io::Error::other)?;
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let defaults = Params::DEFAULT_M_COST;
    let header = Header {
        format: 2,
        cipher: "xchacha20poly1305-stream".to_owned(),
        kdf: "argon2id".to_owned(),
        m_cost: defaults,
        t_cost: Params::DEFAULT_T_COST,
        p_cost: Params::DEFAULT_P_COST,
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        meta,
    };
    let derived = derive(key, &header).map_err(io::Error::other)?;
    let json = serde_json::to_vec(&header).map_err(io::Error::other)?;
    let mut aad = Vec::with_capacity(MAGIC.len() + 4 + json.len());
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(&(json.len() as u32).to_be_bytes());
    aad.extend_from_slice(&json);
    out.write_all(&aad)?;
    let cipher = XChaCha20Poly1305::new(CipherKey::from_slice(&derived));
    Ok(EncryptWriter {
        out,
        enc: Some(EncryptorBE32::from_aead(cipher, (&nonce).into())),
        buf: Vec::with_capacity(CHUNK),
        aad,
    })
}

pub struct EncryptWriter<W: Write> {
    out: W,
    enc: Option<EncryptorBE32<XChaCha20Poly1305>>,
    buf: Vec<u8>,
    aad: Vec<u8>,
}

impl<W: Write> std::fmt::Debug for EncryptWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptWriter").finish_non_exhaustive()
    }
}

impl<R: Read> std::fmt::Debug for DecryptReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecryptReader").finish_non_exhaustive()
    }
}

impl<W: Write> EncryptWriter<W> {
    /// Seals what is left as the last chunk. Without this the file reads as cut short.
    pub fn finish(mut self) -> io::Result<W> {
        let enc = self.enc.take().expect("finish is called once");
        let sealed = enc
            .encrypt_last(Payload {
                msg: &self.buf,
                aad: &self.aad,
            })
            .map_err(|_| io::Error::other("couldn't seal the last chunk"))?;
        self.out.write_all(&sealed)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for EncryptWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut rest = data;
        while !rest.is_empty() {
            // A full chunk waits until more data proves it isn't the last one.
            if self.buf.len() == CHUNK {
                let enc = self.enc.as_mut().expect("not finished");
                let sealed = enc
                    .encrypt_next(Payload {
                        msg: &self.buf,
                        aad: &self.aad,
                    })
                    .map_err(|_| io::Error::other("couldn't seal a chunk"))?;
                self.out.write_all(&sealed)?;
                self.buf.clear();
            }
            let take = (CHUNK - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads the header of an encrypted backup, leaving `input` at the first chunk.
pub fn read_header<R: Read>(input: &mut R) -> io::Result<(Header, Vec<u8>)> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
    let mut magic = [0_u8; 8];
    input
        .read_exact(&mut magic)
        .map_err(|_| bad("too short to be a backup"))?;
    if &magic != MAGIC {
        return Err(bad("not an encrypted Irori backup"));
    }
    let mut len = [0_u8; 4];
    input
        .read_exact(&mut len)
        .map_err(|_| bad("the header is cut short"))?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_HEADER {
        return Err(bad("the header's length is wrong"));
    }
    let mut json = vec![0_u8; len];
    input
        .read_exact(&mut json)
        .map_err(|_| bad("the header is cut short"))?;
    let header: Header = serde_json::from_slice(&json).map_err(|_| bad("the header is damaged"))?;
    if header.format != 2 || header.cipher != "xchacha20poly1305-stream" || header.kdf != "argon2id"
    {
        return Err(bad(
            "this backup was encrypted in a way this Irori can't read",
        ));
    }
    let mut aad = Vec::with_capacity(12 + len);
    aad.extend_from_slice(&magic);
    aad.extend_from_slice(&(len as u32).to_be_bytes());
    aad.extend_from_slice(&json);
    Ok((header, aad))
}

/// Opens the body that follows `header`.
pub fn decrypt_from<R: Read>(
    input: R,
    key: &str,
    header: &Header,
    aad: Vec<u8>,
) -> Result<DecryptReader<R>, String> {
    let derived = derive(key, header)?;
    let nonce = B64
        .decode(&header.nonce)
        .ok()
        .filter(|n| n.len() == NONCE)
        .ok_or("the header's nonce is damaged")?;
    let cipher = XChaCha20Poly1305::new(CipherKey::from_slice(&derived));
    Ok(DecryptReader {
        input,
        dec: Some(DecryptorBE32::from_aead(cipher, nonce.as_slice().into())),
        aad,
        ahead: None,
        plain: Vec::new(),
        at: 0,
    })
}

pub struct DecryptReader<R: Read> {
    input: R,
    dec: Option<DecryptorBE32<XChaCha20Poly1305>>,
    aad: Vec<u8>,
    /// The next block, read before the current one is opened, so the last is known to be last.
    ahead: Option<Vec<u8>>,
    plain: Vec<u8>,
    at: usize,
}

impl<R: Read> DecryptReader<R> {
    fn block(&mut self) -> io::Result<Vec<u8>> {
        let mut block = vec![0_u8; CHUNK + TAG];
        let mut filled = 0;
        while filled < block.len() {
            match self.input.read(&mut block[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        block.truncate(filled);
        Ok(block)
    }

    /// Opens the next block into `plain`. `false` when there is none.
    fn refill(&mut self) -> io::Result<bool> {
        let wrong = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong key, or the backup is damaged or cut short",
            )
        };
        if self.dec.is_none() {
            return Ok(false);
        }
        let current = match self.ahead.take() {
            Some(block) => block,
            None => self.block()?,
        };
        if current.is_empty() {
            // Nothing after the header.
            return Err(wrong());
        }
        let next = self.block()?;
        let payload = Payload {
            msg: &current,
            aad: &self.aad,
        };
        if next.is_empty() {
            let dec = self.dec.take().expect("checked above");
            self.plain = dec.decrypt_last(payload).map_err(|_| wrong())?;
        } else {
            let dec = self.dec.as_mut().expect("checked above");
            self.plain = dec.decrypt_next(payload).map_err(|_| wrong())?;
            self.ahead = Some(next);
        }
        self.at = 0;
        Ok(true)
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            if !self.refill()? {
                return Ok(0);
            }
        }
        let n = (self.plain.len() - self.at).min(out.len());
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::Manifest;

    fn meta() -> Manifest {
        Manifest {
            format: 2,
            irori_version: "1.0.0".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            extensions: vec![],
            encrypted: true,
            history: true,
        }
    }

    fn seal(data: &[u8], key: &str) -> Vec<u8> {
        let mut w = encrypt_to(Vec::new(), key, meta()).expect("start");
        w.write_all(data).expect("write");
        w.finish().expect("finish")
    }

    fn open(sealed: &[u8], key: &str) -> io::Result<Vec<u8>> {
        let mut input = sealed;
        let (header, aad) = read_header(&mut input)?;
        let mut r = decrypt_from(input, key, &header, aad).map_err(io::Error::other)?;
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn what_goes_in_comes_out_for_every_size_around_a_chunk() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 7] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            let key = generate_key();
            let sealed = seal(&data, &key);
            if len == 0 {
                // An empty body still has a sealed last chunk (just the tag).
                assert!(open(&sealed, &key).expect("opens").is_empty());
            } else {
                assert_eq!(open(&sealed, &key).expect("opens"), data, "{len}");
            }
        }
    }

    #[test]
    fn the_wrong_key_a_cut_file_and_an_edited_header_are_all_refused() {
        let data = vec![9_u8; 3 * CHUNK + 5];
        let sealed = seal(&data, "right");
        assert!(open(&sealed, "wrong").is_err());
        // Cut at a chunk boundary and in the middle of one.
        let first_chunk_end = sealed.len() - (CHUNK + TAG) - (5 + TAG);
        assert!(open(&sealed[..first_chunk_end], "right").is_err());
        assert!(open(&sealed[..sealed.len() - 3], "right").is_err());
        // One flipped bit in the body, and one in the header's time.
        let mut flipped = sealed.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(open(&flipped, "right").is_err());
        let mut edited = sealed.clone();
        let at = edited
            .windows(4)
            .position(|w| w == b"2026")
            .expect("the time is in the header");
        edited[at + 3] = b'7';
        assert!(open(&edited, "right").is_err());
    }

    #[test]
    fn a_recovery_key_is_read_loosely_and_a_passphrase_as_typed() {
        let key = generate_key();
        assert_eq!(key.len(), 29);
        assert_eq!(normalize_key(&key.to_lowercase().replace('-', " ")), key);
        assert_eq!(
            normalize_key("correct horse battery"),
            "correct horse battery"
        );
        // The key opens what the typed-loosely form sealed.
        let sealed = seal(b"x", &key);
        assert_eq!(open(&sealed, &key.to_lowercase()).expect("opens"), b"x");
    }
}

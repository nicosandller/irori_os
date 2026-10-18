//! Where a backup is copied to besides this machine (`docs/specs/backup.md` §5): a folder (which
//! is how a mounted network share or a USB disk is reached), a WebDAV folder, or anything that
//! speaks S3.
//!
//! Only what this module wrote is ever touched there: a name has to pass `is_backup_name`, so a
//! destination that is a folder with other things in it loses none of them to retention.
//! Everything here blocks; the callers are already on a thread of their own.

use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use super::is_backup_name;
use super::settings::Destination;

/// A backup that is at a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
}

/// A client for small requests (listing, deleting, making a folder): a server that doesn't
/// answer in a minute is not going to.
fn client() -> Result<reqwest::blocking::Client, String> {
    client_for(Duration::from_secs(60))
}

fn client_for(total: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(total)
        .build()
        .map_err(|e| e.to_string())
}

/// How long moving `bytes` may take: two minutes, and room for a link no slower than
/// 256 KiB/s. A hung server then holds a thread for as long as the file warrants, not for hours.
fn transfer_time(bytes: u64) -> Duration {
    Duration::from_secs(120 + bytes / (256 * 1024))
}

/// For a download, whose size isn't known until it starts.
fn long_transfer() -> Result<reqwest::blocking::Client, String> {
    client_for(Duration::from_secs(6 * 3600))
}

fn said(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn status_text(response: &reqwest::blocking::Response, what: &str) -> String {
    format!("{what}: the server answered {}", response.status())
}

/// Copies `file` to the destination under `name`.
pub fn put(
    dest: &Destination,
    secret: Option<&str>,
    file: &Path,
    name: &str,
) -> Result<(), String> {
    if !is_backup_name(name) {
        return Err(format!("`{name}` isn't a backup's name"));
    }
    match dest {
        Destination::Path { path, .. } => put_path(path, file, name),
        Destination::Webdav { url, username, .. } => {
            put_webdav(url, username, secret.unwrap_or(""), file, name)
        }
        Destination::S3 { .. } => put_s3(dest, secret.unwrap_or(""), file, name),
    }
}

/// The backups at the destination.
pub fn list(dest: &Destination, secret: Option<&str>) -> Result<Vec<Remote>, String> {
    let mut found = match dest {
        Destination::Path { path, .. } => list_path(path)?,
        Destination::Webdav { url, username, .. } => {
            list_webdav(url, username, secret.unwrap_or(""))?
        }
        Destination::S3 { .. } => list_s3(dest, secret.unwrap_or(""))?,
    };
    found.retain(|r| is_backup_name(&r.name));
    found.sort_by(|a, b| b.name.cmp(&a.name));
    found.dedup();
    Ok(found)
}

pub fn delete(dest: &Destination, secret: Option<&str>, name: &str) -> Result<(), String> {
    if !is_backup_name(name) {
        return Err(format!("`{name}` isn't a backup's name"));
    }
    match dest {
        Destination::Path { path, .. } => fs::remove_file(path.join(name)).map_err(said),
        Destination::Webdav { url, username, .. } => {
            let response = client()?
                .delete(join(url, name))
                .basic_auth(username, secret)
                .send()
                .map_err(said)?;
            if response.status().is_success() || response.status().as_u16() == 404 {
                Ok(())
            } else {
                Err(status_text(&response, "couldn't delete it"))
            }
        }
        Destination::S3 { .. } => {
            let response = s3_request(
                dest,
                secret.unwrap_or(""),
                "DELETE",
                &s3_key(dest, name),
                &[],
                None,
                client()?,
            )?;
            if response.status().is_success() || response.status().as_u16() == 404 {
                Ok(())
            } else {
                Err(status_text(&response, "couldn't delete it"))
            }
        }
    }
}

/// Brings a backup from the destination to `to`.
pub fn get(dest: &Destination, secret: Option<&str>, name: &str, to: &Path) -> Result<(), String> {
    if !is_backup_name(name) {
        return Err(format!("`{name}` isn't a backup's name"));
    }
    let part = to.with_extension("part");
    let result = (|| -> Result<(), String> {
        match dest {
            Destination::Path { path, .. } => {
                fs::copy(path.join(name), &part).map_err(said)?;
            }
            Destination::Webdav { url, username, .. } => {
                let mut response = long_transfer()?
                    .get(join(url, name))
                    .basic_auth(username, secret)
                    .send()
                    .map_err(said)?;
                if !response.status().is_success() {
                    return Err(status_text(&response, "couldn't fetch it"));
                }
                let mut out = fs::File::create(&part).map_err(said)?;
                response.copy_to(&mut out).map_err(said)?;
            }
            Destination::S3 { .. } => {
                let mut response = s3_request(
                    dest,
                    secret.unwrap_or(""),
                    "GET",
                    &s3_key(dest, name),
                    &[],
                    None,
                    long_transfer()?,
                )?;
                if !response.status().is_success() {
                    return Err(status_text(&response, "couldn't fetch it"));
                }
                let mut out = fs::File::create(&part).map_err(said)?;
                response.copy_to(&mut out).map_err(said)?;
            }
        }
        fs::rename(&part, to).map_err(said)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Whether the destination can be reached and written to, said in words.
pub fn check(dest: &Destination, secret: Option<&str>) -> Result<(), String> {
    match dest {
        Destination::Path { path, .. } => {
            if !path.is_dir() {
                return Err(format!(
                    "{} isn't a folder this machine can see. Is the share or the disk mounted?",
                    path.display()
                ));
            }
            let probe = path.join(".irori-write-test");
            fs::write(&probe, b"x").map_err(|e| format!("can't write there: {e}"))?;
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        _ => list(dest, secret).map(|_| ()),
    }
}

// ----- a folder -----

fn put_path(dir: &Path, file: &Path, name: &str) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!(
            "{} isn't a folder this machine can see. Is the share or the disk mounted?",
            dir.display()
        ));
    }
    let part = dir.join(format!(".{name}.part"));
    let result = fs::copy(file, &part)
        .map_err(|e| format!("couldn't copy to {}: {e}", dir.display()))
        .and_then(|_| fs::rename(&part, dir.join(name)).map_err(said));
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn list_path(dir: &Path) -> Result<Vec<Remote>, String> {
    let read = fs::read_dir(dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    Ok(read
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .map(|n| Remote { name: n.to_owned() })
        })
        .collect())
}

// ----- WebDAV -----

fn join(base: &str, name: &str) -> String {
    format!("{}/{name}", base.trim_end_matches('/'))
}

fn put_webdav(
    base: &str,
    user: &str,
    password: &str,
    file: &Path,
    name: &str,
) -> Result<(), String> {
    let length = fs::metadata(file).map_err(said)?.len();
    let client = client_for(transfer_time(length))?;
    // Sent under a name nobody lists, then moved into place: a connection that drops half way
    // leaves a stray part, never a short file with a backup's name that retention would count.
    let part = format!(".{name}.part");
    let send = || -> Result<reqwest::blocking::Response, String> {
        let source = fs::File::open(file).map_err(said)?;
        client
            .put(join(base, &part))
            .basic_auth(user, Some(password))
            .body(reqwest::blocking::Body::sized(source, length))
            .send()
            .map_err(said)
    };
    let mut response = send()?;
    // The folder isn't there yet: make it, once, and try again.
    if response.status().as_u16() == 409 || response.status().as_u16() == 404 {
        let made = client
            .request(
                reqwest::Method::from_bytes(b"MKCOL").map_err(said)?,
                base.trim_end_matches('/'),
            )
            .basic_auth(user, Some(password))
            .send()
            .map_err(said)?;
        if made.status().is_success() {
            response = send()?;
        }
    }
    if response.status().as_u16() == 401 {
        return Err("the server refused the user name or password".to_owned());
    }
    if !response.status().is_success() {
        return Err(status_text(&response, "couldn't copy it there"));
    }
    let moved = client
        .request(
            reqwest::Method::from_bytes(b"MOVE").map_err(said)?,
            join(base, &part),
        )
        .basic_auth(user, Some(password))
        .header("Destination", join(base, name))
        .header("Overwrite", "T")
        .send()
        .map_err(said);
    match moved {
        Ok(r) if r.status().is_success() => Ok(()),
        other => {
            let why = match other {
                Ok(r) => status_text(&r, "couldn't put it in place"),
                Err(e) => e,
            };
            let _ = client
                .delete(join(base, &part))
                .basic_auth(user, Some(password))
                .send();
            Err(why)
        }
    }
}

fn list_webdav(base: &str, user: &str, password: &str) -> Result<Vec<Remote>, String> {
    let response = client()?
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").map_err(said)?,
            base.trim_end_matches('/').to_owned() + "/",
        )
        .basic_auth(user, Some(password))
        .header("Depth", "1")
        .header("Content-Type", "application/xml")
        .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#)
        .send()
        .map_err(said)?;
    if response.status().as_u16() == 401 {
        return Err("the server refused the user name or password".to_owned());
    }
    if response.status().as_u16() == 404 {
        // Not made yet: nothing is there.
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        return Err(status_text(&response, "couldn't look there"));
    }
    let body = response.text().map_err(said)?;
    Ok(tag_values(&body, "href")
        .into_iter()
        .filter_map(|href| {
            let last = href.trim_end_matches('/').rsplit('/').next()?.to_owned();
            Some(Remote {
                name: percent_decode(&last),
            })
        })
        .collect())
}

/// The text of every `<…:tag>…</…:tag>` in `xml`, whatever the namespace prefix is.
fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let name = rest[..close].trim_end_matches('/');
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == tag && !name.starts_with('/') {
            let after = &rest[close + 1..];
            if let Some(end) = after.find("</") {
                out.push(xml_unescape(after[..end].trim()));
            }
        }
        rest = &rest[close + 1..];
    }
    out
}

/// The five entities XML names; anything else is left as it was.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Worked out on bytes: the two after a `%` need not be a whole character, and a name
        // that came from a server is not to bring this down.
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|pair| Some((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?));
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

// ----- S3 -----

fn s3_parts(dest: &Destination) -> (&str, &str, &str, &str, &str) {
    match dest {
        Destination::S3 {
            endpoint,
            bucket,
            region,
            prefix,
            access_key,
            ..
        } => (endpoint, bucket, region, prefix, access_key),
        _ => ("", "", "", "", ""),
    }
}

fn s3_key(dest: &Destination, name: &str) -> String {
    let (_, _, _, prefix, _) = s3_parts(dest);
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn put_s3(dest: &Destination, secret: &str, file: &Path, name: &str) -> Result<(), String> {
    // The payload's hash is part of the signature, so the file is read once to hash it and once
    // to send it.
    let mut source = fs::File::open(file).map_err(said)?;
    let length = source.metadata().map_err(said)?.len();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let n = source.read(&mut buffer).map_err(said)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash = hex(&hasher.finalize());
    let body = reqwest::blocking::Body::sized(fs::File::open(file).map_err(said)?, length);
    let response = s3_request(
        dest,
        secret,
        "PUT",
        &s3_key(dest, name),
        &[],
        Some((hash, body)),
        client_for(transfer_time(length))?,
    )?;
    if response.status().is_success() {
        Ok(())
    } else if matches!(response.status().as_u16(), 401 | 403) {
        Err("the storage refused the access key or the secret".to_owned())
    } else {
        Err(status_text(&response, "couldn't copy it there"))
    }
}

fn list_s3(dest: &Destination, secret: &str) -> Result<Vec<Remote>, String> {
    let (_, _, _, prefix, _) = s3_parts(dest);
    let prefix = {
        let p = prefix.trim_matches('/');
        if p.is_empty() {
            String::new()
        } else {
            format!("{p}/")
        }
    };
    let mut found = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..50 {
        let mut query = vec![
            ("list-type".to_owned(), "2".to_owned()),
            ("prefix".to_owned(), prefix.clone()),
        ];
        if let Some(t) = &token {
            query.push(("continuation-token".to_owned(), t.clone()));
        }
        let response = s3_request(dest, secret, "GET", "", &query, None, client()?)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err("the storage refused the access key or the secret".to_owned());
        }
        if response.status().as_u16() == 404 {
            return Err("that bucket isn't there".to_owned());
        }
        if !response.status().is_success() {
            return Err(status_text(&response, "couldn't look there"));
        }
        let body = response.text().map_err(said)?;
        for key in tag_values(&body, "Key") {
            let name = key.strip_prefix(&prefix).unwrap_or(&key);
            if !name.contains('/') {
                found.push(Remote {
                    name: name.to_owned(),
                });
            }
        }
        let more = tag_values(&body, "IsTruncated")
            .first()
            .is_some_and(|v| v == "true");
        token = tag_values(&body, "NextContinuationToken")
            .into_iter()
            .next();
        if !(more && token.is_some()) {
            break;
        }
    }
    Ok(found)
}

/// One signed request to the bucket, path-style (`endpoint/bucket/key`), which every S3 service
/// takes. `payload` is its hash and body; without one the body is empty.
fn s3_request(
    dest: &Destination,
    secret: &str,
    method: &str,
    key: &str,
    query: &[(String, String)],
    payload: Option<(String, reqwest::blocking::Body)>,
    client: reqwest::blocking::Client,
) -> Result<reqwest::blocking::Response, String> {
    let (endpoint, bucket, region, _, access) = s3_parts(dest);
    let mut url = reqwest::Url::parse(endpoint.trim_end_matches('/'))
        .map_err(|e| format!("the endpoint isn't an address: {e}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "the endpoint isn't an address".to_owned())?;
        segments.pop_if_empty().push(bucket);
        for part in key.split('/').filter(|p| !p.is_empty()) {
            segments.push(part);
        }
    }
    if !query.is_empty() {
        let mut sorted: Vec<_> = query.to_vec();
        sorted.sort();
        url.query_pairs_mut().clear().extend_pairs(sorted);
    }
    let hash = payload
        .as_ref()
        .map(|(h, _)| h.clone())
        .unwrap_or_else(|| hex(&Sha256::digest(b"")));
    let now = jiff::Timestamp::now();
    let headers = sign(method, &url, region, access, secret, &hash, now);
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(said)?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some((_, body)) = payload {
        request = request.body(body);
    }
    request.send().map_err(said)
}

/// The headers AWS Signature Version 4 asks for: `host`, `x-amz-content-sha256`, `x-amz-date`
/// and `authorization`.
fn sign(
    method: &str,
    url: &reqwest::Url,
    region: &str,
    access: &str,
    secret: &str,
    payload_hash: &str,
    now: jiff::Timestamp,
) -> Vec<(String, String)> {
    let stamp = now.strftime("%Y%m%dT%H%M%SZ").to_string();
    let day = &stamp[..8];
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => String::new(),
    };
    let canonical_query = {
        let mut pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (uri_encode(&k), uri_encode(&v)))
            .collect();
        pairs.sort();
        pairs
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    };
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!(
        "{method}\n{}\n{canonical_query}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{stamp}\n\n{signed}\n{payload_hash}",
        canonical_path(url)
    );
    let scope = format!("{day}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), day.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
    vec![
        ("x-amz-content-sha256".to_owned(), payload_hash.to_owned()),
        ("x-amz-date".to_owned(), stamp),
        (
            "authorization".to_owned(),
            format!(
                "AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={signature}"
            ),
        ),
    ]
}

/// The path as SigV4 writes it for S3: each segment encoded once, with everything but the
/// unreserved characters as `%XX`. (`Url` leaves `!'()*+,;=:@` alone, and AWS does not.)
fn canonical_path(url: &reqwest::Url) -> String {
    let segments: Vec<String> = url
        .path_segments()
        .map(|parts| parts.map(|p| uri_encode(&percent_decode(p))).collect())
        .unwrap_or_default();
    format!("/{}", segments.join("/"))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut key = if key.len() > BLOCK {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    key.resize(BLOCK, 0);
    let mut inner = Sha256::new();
    inner.update(key.iter().map(|b| b ^ 0x36).collect::<Vec<_>>());
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(key.iter().map(|b| b ^ 0x5c).collect::<Vec<_>>());
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Percent-encoding as SigV4 wants it: everything but unreserved characters.
fn uri_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// When a backup was made, from its name: `irori-backup-20260102-030000[-2].tar.gz[.enc]`. For
/// retention at a place where a file's own modified time isn't to be had.
pub fn time_from_name(name: &str) -> Option<jiff::Timestamp> {
    let stamp = name.strip_prefix(super::PREFIX)?.get(..15)?;
    let (date, time) = stamp.split_once('-')?;
    let text = format!(
        "{}-{}-{}T{}:{}:{}Z",
        date.get(..4)?,
        date.get(4..6)?,
        date.get(6..8)?,
        time.get(..2)?,
        time.get(2..4)?,
        time.get(4..6)?
    );
    text.parse().ok()
}

/// A copy of `names` retention would keep, newest first: drops what is past `keep`/`keep_days`.
pub fn expired_at(remote: &[Remote], keep: u32, keep_days: Option<u32>) -> Vec<String> {
    let mut dated: Vec<(String, jiff::Timestamp)> = remote
        .iter()
        .filter_map(|r| Some((r.name.clone(), time_from_name(&r.name)?)))
        .collect();
    dated.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    super::expired(
        &dated,
        super::PREFIX,
        keep,
        keep_days,
        jiff::Timestamp::now(),
    )
}

/// Basic auth's header value, for tests that look at what was sent.
#[cfg(test)]
fn basic(user: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

#[cfg(test)]
#[path = "dest/tests.rs"]
mod tests;

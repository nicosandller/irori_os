use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::*;

/// A server that is every destination at once: it keeps what is PUT, and answers the listing
/// a WebDAV client (PROPFIND) and an S3 client (`list-type=2`) each ask for. It remembers the
/// requests, so a test can look at what was sent.
#[derive(Default)]
struct Seen {
    files: BTreeMap<String, Vec<u8>>,
    requests: Vec<(String, String, BTreeMap<String, String>)>,
}

fn serve() -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("an address");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let kept = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let kept = Arc::clone(&kept);
            std::thread::spawn(move || answer(stream, &kept));
        }
    });
    (format!("http://{address}"), seen)
}

fn answer(stream: TcpStream, seen: &Mutex<Seen>) {
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (
            parts.next().unwrap_or("").to_owned(),
            parts.next().unwrap_or("").to_owned(),
        );
        let mut headers = BTreeMap::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap_or(0);
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body).expect("the body");
        let (path, query) = target
            .split_once('?')
            .map_or((target.as_str(), ""), |(p, q)| (p, q));
        let mut state = seen.lock().expect("lock");
        state
            .requests
            .push((method.clone(), target.clone(), headers.clone()));
        let (status, payload): (u16, Vec<u8>) = match method.as_str() {
            "PUT" => {
                state.files.insert(path.to_owned(), body);
                (201, Vec::new())
            }
            "GET" if query.contains("list-type=2") => {
                let prefix_dir = path.trim_end_matches('/');
                let wanted = s3_prefix(query);
                let keys: String = state
                    .files
                    .keys()
                    .filter(|k| k.starts_with(prefix_dir))
                    .map(|k| {
                        k.trim_start_matches(prefix_dir)
                            .trim_start_matches('/')
                            .to_owned()
                    })
                    .filter(|key| key.starts_with(&wanted))
                    .map(|key| format!("<Contents><Key>{key}</Key><Size>1</Size></Contents>"))
                    .collect();
                (200, format!("<ListBucketResult><IsTruncated>false</IsTruncated>{keys}</ListBucketResult>").into_bytes())
            }
            "GET" => match state.files.get(path) {
                Some(bytes) => (200, bytes.clone()),
                None => (404, Vec::new()),
            },
            "DELETE" => {
                state.files.remove(path);
                (204, Vec::new())
            }
            "PROPFIND" => {
                let dir = path.trim_end_matches('/');
                let items: String = state
                    .files
                    .keys()
                    .filter(|k| k.starts_with(dir))
                    .map(|k| {
                        format!(
                            "<d:response><d:href>{}</d:href></d:response>",
                            k.replace(' ', "%20")
                        )
                    })
                    .collect();
                (207, format!("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{dir}/</d:href></d:response>{items}</d:multistatus>").into_bytes())
            }
            _ => (405, Vec::new()),
        };
        drop(state);
        let mut out = reader.get_ref().try_clone().expect("a clone");
        let _ = write!(
            out,
            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n",
            payload.len()
        );
        let _ = out.write_all(&payload);
    }
}

fn s3_prefix(query: &str) -> String {
    query
        .split('&')
        .find_map(|p| p.strip_prefix("prefix="))
        .map(percent_decode)
        .unwrap_or_default()
}

fn file(dir: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("a file");
    path
}

const NAME: &str = "irori-backup-20260102-030000.tar.gz.enc";

#[test]
fn a_folder_takes_a_copy_lists_it_and_leaves_what_is_not_ours() {
    let source = tempfile::tempdir().expect("dir");
    let place = tempfile::tempdir().expect("dir");
    let dest = Destination::Path {
        name: "usb".into(),
        path: place.path().to_owned(),
    };
    fs::write(place.path().join("family-photos.zip"), "keep").expect("write");
    put(&dest, None, &file(source.path(), NAME, b"sealed"), NAME).expect("copied");
    assert_eq!(fs::read(place.path().join(NAME)).expect("read"), b"sealed");
    assert_eq!(
        list(&dest, None).expect("listed"),
        [Remote { name: NAME.into() }]
    );
    assert!(check(&dest, None).is_ok());
    // A name that isn't a backup's is refused before anything happens.
    assert!(delete(&dest, None, "family-photos.zip").is_err());
    delete(&dest, None, NAME).expect("deleted");
    assert!(place.path().join("family-photos.zip").exists());
    let gone = Destination::Path {
        name: "usb".into(),
        path: place.path().join("not-mounted"),
    };
    assert!(check(&gone, None).expect_err("refused").contains("mounted"));
}

#[test]
fn webdav_puts_with_the_password_lists_by_name_and_brings_a_copy_back() {
    let (url, seen) = serve();
    let source = tempfile::tempdir().expect("dir");
    let dest = Destination::Webdav {
        name: "cloud".into(),
        url: format!("{url}/dav/irori/"),
        username: "me".into(),
    };
    put(
        &dest,
        Some("pw"),
        &file(source.path(), NAME, b"sealed-bytes"),
        NAME,
    )
    .expect("copied");
    {
        let state = seen.lock().expect("lock");
        let (method, target, headers) = &state.requests[0];
        assert_eq!(
            (method.as_str(), target.as_str()),
            ("PUT", format!("/dav/irori/{NAME}").as_str())
        );
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some(basic("me", "pw").as_str())
        );
        assert_eq!(state.files[&format!("/dav/irori/{NAME}")], b"sealed-bytes");
    }
    assert_eq!(
        list(&dest, Some("pw")).expect("listed"),
        [Remote { name: NAME.into() }]
    );
    let back = source.path().join("back.tar.gz.enc");
    get(&dest, Some("pw"), NAME, &back).expect("fetched");
    assert_eq!(fs::read(&back).expect("read"), b"sealed-bytes");
    delete(&dest, Some("pw"), NAME).expect("deleted");
    assert!(list(&dest, Some("pw")).expect("listed").is_empty());
}

#[test]
fn s3_signs_what_it_sends_and_keeps_to_its_prefix() {
    let (url, seen) = serve();
    let source = tempfile::tempdir().expect("dir");
    let dest = Destination::S3 {
        name: "b2".into(),
        endpoint: url,
        bucket: "irori".into(),
        region: "us-west-1".into(),
        prefix: "home/".into(),
        access_key: "AK".into(),
    };
    put(
        &dest,
        Some("SK"),
        &file(source.path(), NAME, b"sealed-bytes"),
        NAME,
    )
    .expect("copied");
    {
        let state = seen.lock().expect("lock");
        let (method, target, headers) = &state.requests[0];
        assert_eq!(
            (method.as_str(), target.as_str()),
            ("PUT", format!("/irori/home/{NAME}").as_str())
        );
        let auth = &headers["authorization"];
        assert!(
            auth.starts_with("AWS4-HMAC-SHA256 Credential=AK/")
                && auth.contains("/us-west-1/s3/aws4_request"),
            "{auth}"
        );
        assert!(
            auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"),
            "{auth}"
        );
        // The hash it signed is the hash of the bytes it sent.
        assert_eq!(
            headers["x-amz-content-sha256"],
            hex(&Sha256::digest(b"sealed-bytes"))
        );
        assert_eq!(state.files[&format!("/irori/home/{NAME}")], b"sealed-bytes");
    }
    assert_eq!(
        list(&dest, Some("SK")).expect("listed"),
        [Remote { name: NAME.into() }]
    );
    let back = source.path().join("back");
    get(&dest, Some("SK"), NAME, &back).expect("fetched");
    assert_eq!(fs::read(&back).expect("read"), b"sealed-bytes");
    delete(&dest, Some("SK"), NAME).expect("deleted");
}

/// The signature for a request fixed in time, worked out separately (Python's hmac and hashlib,
/// from the written definition of Signature Version 4) rather than by this code.
#[test]
fn the_signature_is_what_signature_version_4_says_it_is() {
    let url = reqwest::Url::parse(
        "https://s3.example.test:9000/bucket/irori/x.tar.gz.enc?list-type=2&prefix=irori%2F",
    )
    .expect("a url");
    let now: jiff::Timestamp = "2026-03-05T04:05:06Z".parse().expect("time");
    let headers = sign(
        "GET",
        &url,
        "us-east-1",
        "AK",
        "SK",
        &hex(&Sha256::digest(b"")),
        now,
    );
    let auth = &headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .expect("signed")
        .1;
    assert!(
        auth.ends_with(
            "Signature=695125684a27c7ee340ab05315a380457ae080938a2040ef008bde7f031f5d4b"
        ),
        "{auth}"
    );
}

#[test]
fn retention_at_a_place_goes_by_the_time_in_the_name() {
    let names = [
        "irori-backup-20260105-030000.tar.gz.enc",
        "irori-backup-20260104-030000.tar.gz.enc",
        "irori-backup-20260103-030000-2.tar.gz.enc",
        "irori-backup-20260102-030000.tar.gz.enc",
    ];
    let remote: Vec<Remote> = names.iter().map(|n| Remote { name: (*n).into() }).collect();
    assert_eq!(
        expired_at(&remote, 2, None),
        [names[2], names[3]].map(String::from)
    );
    assert!(time_from_name("secrets.toml").is_none());
    assert!(time_from_name(names[2]).is_some());
}

#[test]
fn xml_is_read_whatever_the_prefix_and_names_come_back_decoded() {
    let xml = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/irori/</D:href></D:response><D:response><D:href>/dav/irori/irori-backup-1.tar.gz%2Benc</D:href></D:response></D:multistatus>"#;
    assert_eq!(
        tag_values(xml, "href"),
        ["/dav/irori/", "/dav/irori/irori-backup-1.tar.gz%2Benc"]
    );
    assert_eq!(percent_decode("a%2Bb%20c"), "a+b c");
}

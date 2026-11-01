//! ONVIF's SOAP: a request envelope with the login in it, and reading the answer.
//!
//! The login is WS-Security's UsernameToken with a password digest,
//! `base64(sha1(nonce + created + password))`. `created` is the camera's time, not ours: a camera
//! whose clock is off refuses a token it thinks is stale, so the difference is measured first
//! (`GetSystemDateAndTime` needs no login) and added.
//!
//! Elements are matched by local name only. Vendors put the same element in different
//! namespaces, and nothing here needs to tell them apart.

use std::time::Duration;

use base64::Engine as _;
use hyper::Method;
use sha1::{Digest as _, Sha1};

use crate::http::{Camera, HttpError, Target};
use crate::settings::Login;

/// SOAP 1.2's content type. The action goes in it too, which some cameras insist on.
const CONTENT_TYPE: &str = "application/soap+xml; charset=utf-8";

/// The camera's clock less ours, so a token is dated in its time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockOffset(pub i64);

/// One camera's ONVIF services, with its login.
#[derive(Debug, Clone)]
pub struct Soap {
    pub camera: Camera,
    pub login: Option<Login>,
    pub offset: ClockOffset,
}

impl Soap {
    /// Sends `body` (what goes inside `<s:Body>`) to `target`, and returns the answer. A fault
    /// is an error: [`HttpError::Refused`] when it's about the login, else
    /// [`HttpError::Failed`] with the camera's reason.
    pub async fn call(
        &self,
        target: &Target,
        action: &str,
        body: &str,
    ) -> Result<String, HttpError> {
        self.call_to(target, action, body, None).await
    }

    /// The same, with WS-Addressing's `Action` and `To` in the header: an event subscription
    /// is told apart from the rest of the camera's service by the address it was given.
    pub async fn call_to(
        &self,
        target: &Target,
        action: &str,
        body: &str,
        to: Option<&str>,
    ) -> Result<String, HttpError> {
        let addressing = to.map(|to| {
            format!(
                "<wsa:Action>{}</wsa:Action><wsa:To>{}</wsa:To>",
                escape(action),
                escape(to)
            )
        });
        let envelope = envelope(
            self.login.as_ref(),
            self.offset,
            addressing.as_deref().unwrap_or_default(),
            body,
        );
        let content_type = format!("{CONTENT_TYPE}; action=\"{action}\"");
        let response = self
            .camera
            .send(
                Method::POST,
                target,
                Some((&content_type, envelope.into_bytes())),
                self.login.as_ref(),
            )
            .await?;
        let text = String::from_utf8(response.body)
            .map_err(|_| HttpError::Failed("the camera's answer isn't text".into()))?;
        if let Some(fault) = fault(&text) {
            return Err(fault);
        }
        if !(200..300).contains(&response.status) {
            return Err(HttpError::Failed(format!(
                "the camera answered {}",
                response.status
            )));
        }
        Ok(text)
    }
}

/// A whole SOAP 1.2 envelope around `body`, with the login's token when there is one, and
/// `extra` in the header too.
pub fn envelope(login: Option<&Login>, offset: ClockOffset, extra: &str, body: &str) -> String {
    let security = match login {
        Some(login) => {
            let mut nonce = [0u8; 16];
            // A failed draw leaves zeros; `created` still makes the digest one-off.
            let _ = getrandom::fill(&mut nonce);
            let created = created_at(offset);
            security(login, &nonce, &created)
        }
        None => String::new(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
         xmlns:tds=\"http://www.onvif.org/ver10/device/wsdl\" \
         xmlns:trt=\"http://www.onvif.org/ver10/media/wsdl\" \
         xmlns:tptz=\"http://www.onvif.org/ver20/ptz/wsdl\" \
         xmlns:tev=\"http://www.onvif.org/ver10/events/wsdl\" \
         xmlns:tt=\"http://www.onvif.org/ver10/schema\" \
         xmlns:wsnt=\"http://docs.oasis-open.org/wsn/b-2\" \
         xmlns:wsa=\"http://www.w3.org/2005/08/addressing\">\
         <s:Header>{security}{extra}</s:Header><s:Body>{body}</s:Body></s:Envelope>"
    )
}

/// The WS-Security header for one request.
pub(crate) fn security(login: &Login, nonce: &[u8], created: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let digest = password_digest(nonce, created, &login.password);
    format!(
        "<wsse:Security s:mustUnderstand=\"1\" \
         xmlns:wsse=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd\" \
         xmlns:wsu=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd\">\
         <wsse:UsernameToken><wsse:Username>{}</wsse:Username>\
         <wsse:Password Type=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest\">{digest}</wsse:Password>\
         <wsse:Nonce EncodingType=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary\">{}</wsse:Nonce>\
         <wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security>",
        escape(&login.username),
        b64.encode(nonce),
    )
}

/// `base64(sha1(nonce + created + password))`.
pub(crate) fn password_digest(nonce: &[u8], created: &str, password: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(nonce);
    sha.update(created.as_bytes());
    sha.update(password.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sha.finalize())
}

/// Now in the camera's time, as WS-Security writes it.
fn created_at(offset: ClockOffset) -> String {
    let now = jiff::Timestamp::now();
    let shifted = now
        .checked_add(jiff::SignedDuration::from_secs(offset.0))
        .unwrap_or(now);
    shifted.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// How far the camera's clock is from ours, from its `GetSystemDateAndTime` answer.
pub fn clock_offset(answer: &str) -> Option<ClockOffset> {
    let doc = roxmltree::Document::parse(answer).ok()?;
    let utc = find(doc.root(), "UTCDateTime")?;
    let number = |parent: &str, field: &str| -> Option<i64> {
        text(find(utc, parent)?, &[field])?.trim().parse().ok()
    };
    let date = jiff::civil::date(
        i16::try_from(number("Date", "Year")?).ok()?,
        i8::try_from(number("Date", "Month")?).ok()?,
        i8::try_from(number("Date", "Day")?).ok()?,
    );
    let time = jiff::civil::Time::new(
        i8::try_from(number("Time", "Hour")?).ok()?,
        i8::try_from(number("Time", "Minute")?).ok()?,
        i8::try_from(number("Time", "Second")?).ok()?,
        0,
    )
    .ok()?;
    let camera = date
        .to_datetime(time)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?;
    let ours = jiff::Timestamp::now();
    Some(ClockOffset(
        camera.timestamp().as_second() - ours.as_second(),
    ))
}

/// A fault in the answer, as an error. A login fault is a refusal; any other is the camera's
/// reason, cut short.
pub fn fault(answer: &str) -> Option<HttpError> {
    let doc = roxmltree::Document::parse(answer).ok()?;
    let fault = find(doc.root(), "Fault")?;
    let codes: Vec<String> = fault
        .descendants()
        .filter(|node| matches!(node.tag_name().name(), "Value" | "faultcode" | "Subcode"))
        .filter_map(|node| node.text())
        .map(|text| text.trim().to_owned())
        .collect();
    let about_login = codes.iter().any(|code| {
        let local = code.rsplit(':').next().unwrap_or(code);
        matches!(
            local,
            "NotAuthorized"
                | "FailedAuthentication"
                | "InvalidSecurityToken"
                | "Sender.NotAuthorized"
        )
    });
    if about_login {
        return Some(HttpError::Refused);
    }
    let reason = find(fault, "Text")
        .or_else(|| find(fault, "faultstring"))
        .and_then(|node| node.text())
        .map(|text| text.trim().chars().take(120).collect::<String>())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "the camera turned the request down".into());
    Some(HttpError::Failed(reason))
}

/// The first element under `node` (itself included) with this local name.
pub fn find<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.descendants()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

/// Every element under `node` with this local name.
pub fn find_all<'a, 'i: 'a>(
    node: roxmltree::Node<'a, 'i>,
    name: &'a str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.descendants()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

/// The text at a path of child elements under `node`, trimmed; `None` if missing or blank.
pub fn text(node: roxmltree::Node, path: &[&str]) -> Option<String> {
    let mut at = node;
    for name in path {
        at = at
            .children()
            .find(|child| child.is_element() && child.tag_name().name() == *name)?;
    }
    let text = at.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// An attribute by local name.
pub fn attribute(node: roxmltree::Node, name: &str) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value().trim().to_owned())
}

/// Text made safe to put between tags or in an attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// An ONVIF duration (`PT0.5S`), for a timeout or a move's length.
pub fn duration(value: Duration) -> String {
    let millis = value.as_millis();
    format!("PT{}.{:03}S", millis / 1000, millis % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example most ONVIF clients test against (python-onvif, onvif-zeep).
    #[test]
    fn the_password_digest_matches_a_known_answer() {
        let b64 = base64::engine::general_purpose::STANDARD;
        let nonce = b64.decode("LKqI6G/AikKCQrN0zqZFlg==").expect("base64");
        assert_eq!(
            password_digest(&nonce, "2010-09-16T07:50:45Z", "userpassword"),
            "tuOSpGlFlIXsozq4HFNeeGeFLEI="
        );
    }

    #[test]
    fn a_token_never_carries_the_password_itself() {
        let login = Login {
            username: "admin<&>".into(),
            password: "hunter2!".into(),
        };
        let header = security(&login, &[1; 16], "2026-11-01T00:00:00Z");
        assert!(!header.contains("hunter2"), "{header}");
        assert!(header.contains("<wsse:Username>admin&lt;&amp;&gt;</wsse:Username>"));
    }

    #[test]
    fn a_login_fault_is_a_refusal_and_others_are_reasons() {
        let refused = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:ter="http://www.onvif.org/ver10/error"><s:Body><s:Fault>
            <s:Code><s:Value>s:Sender</s:Value><s:Subcode><s:Value>ter:NotAuthorized</s:Value>
            </s:Subcode></s:Code><s:Reason><s:Text xml:lang="en">Sender not Authorized</s:Text>
            </s:Reason></s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(fault(refused), Some(HttpError::Refused));

        let other = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"><s:Body>
            <s:Fault><s:Code><s:Value>s:Receiver</s:Value><s:Subcode>
            <s:Value>ter:ActionNotSupported</s:Value></s:Subcode></s:Code>
            <s:Reason><s:Text>Optional Action Not Implemented</s:Text></s:Reason>
            </s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(
            fault(other),
            Some(HttpError::Failed("Optional Action Not Implemented".into()))
        );
        assert_eq!(fault("<s:Envelope><s:Body/></s:Envelope>"), None);
    }

    #[test]
    fn the_cameras_clock_is_read_in_utc() {
        let now = jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::from_secs(3600))
            .expect("an hour on")
            .to_zoned(jiff::tz::TimeZone::UTC);
        let answer = format!(
            "<Envelope><Body><GetSystemDateAndTimeResponse><SystemDateAndTime>\
             <UTCDateTime><Time><Hour>{}</Hour><Minute>{}</Minute><Second>{}</Second></Time>\
             <Date><Year>{}</Year><Month>{}</Month><Day>{}</Day></Date></UTCDateTime>\
             </SystemDateAndTime></GetSystemDateAndTimeResponse></Body></Envelope>",
            now.hour(),
            now.minute(),
            now.second(),
            now.year(),
            now.month(),
            now.day()
        );
        let offset = clock_offset(&answer).expect("a time").0;
        assert!((3598..=3601).contains(&offset), "{offset}");
    }

    #[test]
    fn durations_are_written_the_onvif_way() {
        assert_eq!(duration(Duration::from_millis(500)), "PT0.500S");
        assert_eq!(duration(Duration::from_secs(10)), "PT10.000S");
    }
}

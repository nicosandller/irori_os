//! Setting up, as steps on the extension's page: the public key on the person's website and
//! registered with Tesla, signing in, and Irori's key on each car. None of them waits on a car:
//! a sleeping car doesn't hold setup up (Home Assistant's #181487).

use irori_types::{Name, UniqueId, Waiting, WaitingCopy, WaitingLink};
use p256::PublicKey;
use p256::pkcs8::DecodePublicKey;

use crate::api::{Api, ApiError, Region};
use crate::keys::{self, Key};

/// The unique id the account's device has, which its setup steps belong to.
pub const ACCOUNT_DEVICE: &str = "tesla_account";

/// How registering with Tesla went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    /// Tesla has the app's key, and it's this key.
    Yes,
    /// Tesla registered the app, but with another key: the website serves a different one.
    KeyDiffers,
    /// Tesla refused to register the app, in its words per region.
    Refused(String),
}

/// Registers the app with Tesla in both regions (it's harmless to do again) and checks the key
/// Tesla took from the website is `key`. An error is one to try again later, or billing.
pub async fn register(api: Api, domain: String, key: Key) -> Result<Registered, ApiError> {
    let mut registered = Vec::new();
    let mut refusals = Vec::new();
    for region in Region::ALL {
        match api.register(region, &domain).await {
            Ok(()) => registered.push(region),
            Err(
                error @ (ApiError::Billing(_)
                | ApiError::Server(_)
                | ApiError::Unreachable(_)
                | ApiError::RateLimited(_)),
            ) => return Err(error),
            Err(error) => refusals.push(format!("{}: {}", region.label(), error.describe())),
        }
    }
    if registered.is_empty() {
        return Ok(Registered::Refused(refusals.join("; ")));
    }
    for region in registered {
        let tesla_has = api.registered_key(region, &domain).await?;
        if !tesla_has.is_some_and(|has| same_key(&has, &key)) {
            return Ok(Registered::KeyDiffers);
        }
    }
    Ok(Registered::Yes)
}

/// Whether the key Tesla has on file is `key`. Tesla gives it as the point in hex; a PEM is
/// read too, in case that changes.
pub fn same_key(tesla_has: &str, key: &Key) -> bool {
    let tesla_has = tesla_has.trim();
    if tesla_has.contains("BEGIN") {
        return PublicKey::from_public_key_pem(tesla_has).is_ok_and(|has| has == key.public());
    }
    let hex: String = tesla_has
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    hex == key.public_hex()
}

/// Where the website has to serve the public key.
pub fn key_address(domain: &str) -> String {
    format!("https://{domain}{}", keys::WELL_KNOWN)
}

/// Where a person adds Irori's key to a car, on their phone with the Tesla app.
pub fn virtual_key_address(domain: &str, vin: &str) -> String {
    format!("https://www.tesla.com/_ak/{domain}?vin={vin}")
}

fn account_step(reason: String) -> Waiting {
    Waiting {
        unique_id: UniqueId::try_from(ACCOUNT_DEVICE).expect("a valid id"),
        name: Name::try_from("Tesla account").expect("a valid name"),
        reason,
        secret: None,
        link: None,
        copy: None,
    }
}

/// The step that puts the public key on the website. A brand-new app lands here too, since
/// Tesla can't register it until the key is there, so it reads as the next thing to do.
pub fn key_step(domain: &str, key: &Key, why: &Registered) -> Waiting {
    let address = key_address(domain);
    let trouble = match why {
        Registered::Yes => String::new(),
        Registered::KeyDiffers => format!(
            "\nTesla has a different key for {domain} right now. If another program already uses \
             this developer app, paste its private key into Tesla's settings instead: then \
             nothing changes on the website or the car."
        ),
        Registered::Refused(words) => format!(
            "\nTesla's last answer: {words}. If the key is already there, check that {domain} is \
             the app's Allowed Origin on developer.tesla.com."
        ),
    };
    let reason = format!(
        "Put Irori's public key on your website.\n\
         Where: {address}\n\
         What to do: save the key below as a file at exactly that address. Open the link to \
         check it shows the key, then press Check the key again on this page.\n\
         What to expect: Tesla reads the file and registers your app. This step goes away and \
         signing in comes next.{trouble}"
    );
    Waiting {
        link: Some(WaitingLink {
            label: "Open the key's address".into(),
            url: address,
        }),
        copy: Some(WaitingCopy {
            label: "Public key".into(),
            text: key.public_pem(),
        }),
        ..account_step(reason)
    }
}

/// The step that signs in, with the link to Tesla's sign-in page.
pub fn sign_in_step(url: String, again: bool) -> Waiting {
    let first = if again {
        "Tesla signed Irori out, so sign in again."
    } else {
        "Sign in to Tesla."
    };
    let reason = format!(
        "{first}\n\
         What to do: open the link below, sign in, and allow everything Irori asks for.\n\
         What to expect: Tesla sends you to an address on your website, which may show a \
         \"not found\" page. That's fine. Copy the whole address from the browser's address bar \
         and paste it into Finish signing in on this page. Your cars and energy sites then \
         appear here, ready to add."
    );
    Waiting {
        link: Some(WaitingLink {
            label: "Sign in to Tesla".into(),
            url,
        }),
        ..account_step(reason)
    }
}

/// The step that adds Irori's key to a car, so it takes commands. Reading works meanwhile.
pub fn virtual_key_step(domain: &str, vin: &str, name: &str) -> Option<Waiting> {
    Some(Waiting {
        unique_id: UniqueId::try_from(vin).ok()?,
        name: Name::try_from(name.trim())
            .or_else(|_| Name::try_from(vin))
            .ok()?,
        reason: "Add Irori's key to this car so it can take commands. Reading already works.\n\
                 Where: on your phone, with the Tesla app installed.\n\
                 What to do: open the link below and approve it, then tap your key card on the \
                 console when the car asks.\n\
                 What to expect: this step goes away once Tesla reports the key on the car."
            .into(),
        secret: None,
        link: Some(WaitingLink {
            label: "Add the key to the car".into(),
            url: virtual_key_address(domain, vin),
        }),
        copy: None,
    })
}

#[cfg(test)]
mod tests {
    use p256::pkcs8::{EncodePublicKey, LineEnding};

    use super::*;

    #[test]
    fn the_key_tesla_has_is_compared_whatever_its_shape() {
        let key = Key::generate().expect("a key");
        let other = Key::generate().expect("a key");
        assert!(same_key(&key.public_hex(), &key));
        assert!(same_key(
            &format!(" {}\n", key.public_hex().to_uppercase()),
            &key
        ));
        assert!(!same_key(&other.public_hex(), &key));
        let pem = key.public().to_public_key_pem(LineEnding::LF).expect("pem");
        assert!(same_key(&pem, &key));
        assert!(!same_key("", &key));
    }

    #[test]
    fn every_step_is_a_valid_waiting_item() {
        let key = Key::generate().expect("a key");
        let step = key_step("tesla.example.com", &key, &Registered::KeyDiffers);
        assert_eq!(
            step.link.as_ref().map(|link| link.url.as_str()),
            Some("https://tesla.example.com/.well-known/appspecific/com.tesla.3p.public-key.pem")
        );
        assert!(
            step.copy
                .as_ref()
                .is_some_and(|copy| copy.text.contains("PUBLIC KEY"))
        );
        let car =
            virtual_key_step("tesla.example.com", "5YJ3E1EA7KF000001", "Red car").expect("a step");
        assert_eq!(
            car.link.map(|link| link.url),
            Some("https://www.tesla.com/_ak/tesla.example.com?vin=5YJ3E1EA7KF000001".into())
        );
        let signed_out = sign_in_step("https://auth.tesla.com/x".into(), true);
        assert!(signed_out.reason.contains("signed Irori out"));
        for step in [step, signed_out] {
            serde_json::to_value(&step).expect("serialises");
        }
    }
}

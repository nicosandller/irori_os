//! The map (`docs/specs/protocols.md` §6.8): Irori reaches Tesla's servers over the internet,
//! and they reach each car (over its own mobile connection) and each energy site (over the
//! home's internet). Nothing of Tesla's is on the home network as far as Irori is concerned.
//!
//! A car's node says whether it's awake, which is what decides whether a command has to wake it
//! first.

use irori_types::{Network, NetworkLink, NetworkMedium, NetworkNode, NetworkRole, UniqueId};

const IRORI: &str = "irori";
const CLOUD: &str = "cloud";

/// One car or site as the map shows it.
#[derive(Debug, Clone, Copy)]
pub struct Drawn<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// "Model 3, asleep", "Powerwall".
    pub detail: Option<&'a str>,
}

/// The map. `signed_in` is whether Irori reaches Tesla at all.
pub fn draw<'a>(products: impl IntoIterator<Item = Drawn<'a>>, signed_in: bool) -> Network {
    let node = |id: &str, name: &str, role, medium| NetworkNode {
        id: id.to_owned(),
        name: name.to_owned(),
        role,
        medium,
        device_unique_id: None,
        detail: None,
        host: None,
    };
    let link = |from: &str, to: &str| NetworkLink {
        from: from.to_owned(),
        to: to.to_owned(),
        medium: Some(NetworkMedium::Internet),
        quality: None,
        reading: None,
        detail: None,
    };
    let mut nodes = vec![node(IRORI, "Irori", NetworkRole::Controller, None)];
    let mut links = Vec::new();
    if signed_in {
        nodes.push(node(
            CLOUD,
            "Tesla cloud",
            NetworkRole::Cloud,
            Some(NetworkMedium::Internet),
        ));
        links.push(link(CLOUD, IRORI));
        for product in products {
            let Ok(unique_id) = UniqueId::try_from(product.id) else {
                continue;
            };
            nodes.push(NetworkNode {
                device_unique_id: Some(unique_id),
                detail: product.detail.map(str::to_owned),
                ..node(
                    product.id,
                    product.name,
                    NetworkRole::Device,
                    Some(NetworkMedium::Internet),
                )
            });
            links.push(link(product.id, CLOUD));
        }
    }
    Network {
        name: Some("Tesla".to_owned()),
        detail: Some(
            if signed_in {
                "Through Tesla's cloud"
            } else {
                "Not signed in to Tesla"
            }
            .to_owned(),
        ),
        scanned_at: None,
        nodes,
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cars_and_sites_hang_off_tesla_s_cloud() {
        let products = [
            Drawn {
                id: "5YJ3E1EA7KF000001",
                name: "Red car",
                detail: Some("Model 3, asleep"),
            },
            Drawn {
                id: "2252000000000001",
                name: "Home",
                detail: Some("Powerwall"),
            },
        ];
        let map = draw(products, true);
        map.validate().expect("a map the core accepts");
        assert_eq!(map.nodes.len(), 4);
        let car = map
            .links
            .iter()
            .find(|link| link.from == "5YJ3E1EA7KF000001")
            .expect("linked");
        assert_eq!(
            (car.to.as_str(), car.medium),
            ("cloud", Some(NetworkMedium::Internet))
        );
        let node = map
            .nodes
            .iter()
            .find(|node| node.id == "5YJ3E1EA7KF000001")
            .expect("drawn");
        assert_eq!(node.detail.as_deref(), Some("Model 3, asleep"));

        let signed_out = draw(products, false);
        signed_out.validate().expect("a map the core accepts");
        assert_eq!(signed_out.nodes.len(), 1, "only Irori");
    }
}

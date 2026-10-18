//! Irori's own extensions as updates on Irori's device (`update.irori_mqtt`): which installed
//! official extensions have a newer version in this build's catalog, and installing one.
//!
//! They sit beside every device's firmware in the Devices page's list of updates, and an
//! automation can watch them like any other. Installing does what the Extensions page's Update
//! button does. An extension with full access to this machine is the exception: that is approved
//! by a person each time, so it only says here that an update is waiting.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use irori_core::{Core, ExtensionHost, ExtensionOverview};
use irori_protocol::types::ExtensionId;

use crate::packages::Official;

/// The running core and host, for Irori's own device to read and update extensions through. Set
/// once, after the host starts; until then, and in a process that never sets it, the device has
/// no extension updates to show.
pub static RUNNING: OnceLock<(Core, ExtensionHost)> = OnceLock::new();

/// An installed official extension, as an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub id: ExtensionId,
    pub name: String,
    pub installed: String,
    pub latest: String,
    /// Updating it is approved on the Extensions page, not sent from here.
    pub full_access: bool,
}

/// The official extensions that are installed, each with the version it has and the one this
/// build's catalog holds. One whose manifest hasn't been read yet has no version to compare and
/// is left out until it has.
pub fn offered(
    catalog: &[Official],
    installed: &BTreeMap<ExtensionId, ExtensionOverview>,
) -> Vec<Offered> {
    catalog
        .iter()
        .filter_map(|item| {
            let info = installed.get(&item.id)?.info.as_ref()?;
            Some(Offered {
                id: item.id.clone(),
                name: item.name.to_string(),
                installed: info.version.to_string(),
                latest: item.version.clone(),
                full_access: item.full_access,
            })
        })
        .collect()
}

/// What's installed right now. Empty before the host is up.
pub fn offered_now() -> Vec<Offered> {
    RUNNING.get().map_or_else(Vec::new, |(core, _)| {
        offered(&crate::packages::official(), &core.extensions())
    })
}

/// Replaces an installed official extension with the catalog's version and starts it again, as
/// the Extensions page's Update does. Its devices, settings and data stay.
pub async fn update(id: &ExtensionId) -> Result<(), String> {
    crate::backup::schedule::before_update().await?;
    let (_, host) = RUNNING
        .get()
        .ok_or("Irori is still starting; try again in a moment")?;
    let item = crate::packages::official_by_id(id)
        .ok_or_else(|| format!("`{id}` isn't an official extension"))?;
    let stage = host.packages_dir().join(crate::server::staging_dir());
    let staged = tokio::task::spawn_blocking({
        let stage = stage.clone();
        move || crate::packages::install_official(&item, &stage)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
    // What was fetched is what would run. If it asks for full access, a person approves that.
    .and_then(|()| {
        crate::server::unapproved_full_access(&stage, Some(id), false).map_err(|(_, why)| why)
    });
    let placed = match staged {
        Ok(()) => host.update_package(stage.clone()).await.map(|_| ()),
        Err(why) => Err(why),
    };
    if placed.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_installed_extensions_whose_manifest_was_read_are_offered() {
        let catalog = crate::packages::official();
        assert!(offered(&catalog, &BTreeMap::new()).is_empty());
        // Every catalog entry can be an update: its id and version are what the entity is made of.
        for item in &catalog {
            assert!(
                irori_protocol::types::ObjectId::try_from(format!("irori_{}", item.id)).is_ok(),
                "{}",
                item.id
            );
            assert!(!item.version.is_empty());
        }
    }
}

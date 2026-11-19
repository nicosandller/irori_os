//! `todo`: a to-do or shopping list. Home Assistant's `todo`.
//!
//! Its value is how many items are still to do. One that can be added to takes
//! `todo.add_item`. Reading, ticking off and removing its items are not here yet.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

/// The longest item.
pub const ITEM_MOST: usize = 255;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoCapabilities {
    /// Items can be added to it.
    #[serde(default)]
    pub add: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoState {
    /// How many items are still to do.
    pub open: u32,
}

/// What `todo.add_item` adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoAddItem {
    #[schemars(length(min = 1, max = 255))]
    pub item: String,
}

impl TodoAddItem {
    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.item.trim().is_empty() {
            return Err(InvariantError("an item says something".into()));
        }
        if self.item.chars().count() > ITEM_MOST {
            return Err(InvariantError(format!(
                "an item is at most {ITEM_MOST} characters"
            )));
        }
        Ok(())
    }
}

pub(crate) fn primary(state: &TodoState) -> Typed {
    Typed::Number(f64::from(state.open))
}

pub(crate) fn with_primary(value: &Typed) -> Option<TodoState> {
    match value {
        Typed::Number(open)
            if *open >= 0.0 && open.fract() == 0.0 && *open <= f64::from(u32::MAX) =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
            Some(TodoState { open: *open as u32 })
        }
        _ => None,
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::TodoAddItem => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    match name {
        ServiceName::TodoAddItem => Ok(Service::TodoAddItem(super::parse(name, data)?)),
        _ => Err(super::not_mine(name)),
    }
}

/// An added item is one more to do, but the list says so itself: a toggle has nothing to go by.
pub(crate) fn asks_for(_: &Service) -> Option<Typed> {
    None
}

pub(crate) fn supports_service(caps: &TodoCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::TodoAddItem(_) if !caps.add => Err("can't be added to".into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_counts_what_is_left_and_takes_items_where_it_can() {
        assert_eq!(primary(&TodoState { open: 3 }), Typed::Number(3.0));
        assert_eq!(
            with_primary(&Typed::Number(2.0)),
            Some(TodoState { open: 2 })
        );
        assert_eq!(with_primary(&Typed::Number(-1.0)), None);
        assert_eq!(with_primary(&Typed::Number(1.5)), None);

        let milk = Service::TodoAddItem(TodoAddItem {
            item: "Milk".into(),
        });
        assert!(supports_service(&TodoCapabilities { add: true }, &milk).is_ok());
        assert!(supports_service(&TodoCapabilities { add: false }, &milk).is_err());
        assert!(TodoAddItem { item: " ".into() }.validate().is_err());
    }
}

//! An image: no value of its own, only a picture, which is in its drawer like a camera's
//! ([`super::camera_picture`]).

use leptos::prelude::*;

/// Its row says it has a picture; opening the row shows it.
pub(crate) fn image_reading() -> AnyView {
    view! { <span class="reading">"Picture"</span> }.into_any()
}

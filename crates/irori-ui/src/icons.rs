//! The page's own small icons: one stroke weight, one size, the colour of the text around them.
//!
//! Not for extensions' icons, which are theirs and are only ever loaded as images
//! (`devices::icon`).

use leptos::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// The handle on something that can be dragged.
    Grip,
    Add,
    Edit,
    Remove,
    // The sections of Settings.
    System,
    Appearance,
    Assistant,
    Places,
    Users,
    Logs,
    // What the machine is short of, or isn't.
    Memory,
    Disk,
    Processor,
    Temperature,
    // Saving and putting away what was being typed.
    Check,
    Close,
    Gear,
    // A player's buttons.
    Play,
    Pause,
    Stop,
    Previous,
    Next,
    Power,
    Volume,
    Muted,
    Cast,
}

/// What's drawn inside the icon's 24 × 24 box. Written here and nowhere else, so it's safe to
/// hand to the browser as markup.
fn drawing(icon: Icon) -> &'static str {
    match icon {
        Icon::Grip => concat!(
            r#"<g fill="currentColor" stroke="none"><circle cx="9" cy="6" r="1.7"/>"#,
            r#"<circle cx="15" cy="6" r="1.7"/><circle cx="9" cy="12" r="1.7"/>"#,
            r#"<circle cx="15" cy="12" r="1.7"/><circle cx="9" cy="18" r="1.7"/>"#,
            r#"<circle cx="15" cy="18" r="1.7"/></g>"#,
        ),
        Icon::Add => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::Edit => {
            r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/>"#
        }
        Icon::Remove => concat!(
            r#"<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/>"#,
            r#"<path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#,
        ),
        Icon::System => concat!(
            r#"<rect x="3" y="4" width="18" height="7" rx="1.5"/>"#,
            r#"<rect x="3" y="13" width="18" height="7" rx="1.5"/>"#,
            r#"<path d="M7 7.5h.01M7 16.5h.01M11 7.5h6M11 16.5h6"/>"#,
        ),
        Icon::Appearance => concat!(
            r#"<circle cx="12" cy="12" r="3.5"/>"#,
            r#"<path d="M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8"#,
            r#"M16.6 16.6l1.8 1.8M5.6 18.4l1.8-1.8M16.6 7.4l1.8-1.8"/>"#,
        ),
        Icon::Assistant => {
            r#"<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4h13A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-5 4v-4.2A1.5 1.5 0 0 1 4 14.5Z"/>"#
        }
        Icon::Places => {
            r#"<path d="M12 3 3 8l9 5 9-5-9-5Z"/><path d="m3 12.5 9 5 9-5"/><path d="m3 17 9 5 9-5"/>"#
        }
        Icon::Users => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
        Icon::Logs => {
            r#"<rect x="4" y="3" width="16" height="18" rx="1.5"/><path d="M8 8h8M8 12h8M8 16h5"/>"#
        }
        Icon::Memory => concat!(
            r#"<rect x="3" y="7" width="18" height="10" rx="1.5"/>"#,
            r#"<path d="M7 10.5v3M11 10.5v3M15 10.5v3M6 17v2.5M10 17v2.5M14 17v2.5M18 17v2.5"/>"#,
        ),
        Icon::Disk => concat!(
            r#"<ellipse cx="12" cy="6" rx="8" ry="3"/>"#,
            r#"<path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>"#,
        ),
        Icon::Processor => concat!(
            r#"<rect x="5" y="5" width="14" height="14" rx="1.5"/>"#,
            r#"<rect x="9.5" y="9.5" width="5" height="5"/>"#,
            r#"<path d="M9 2.5V5M15 2.5V5M9 19v2.5M15 19v2.5M2.5 9H5M2.5 15H5M19 9h2.5M19 15h2.5"/>"#,
        ),
        Icon::Temperature => {
            r#"<path d="M14 14.8V5a2 2 0 0 0-4 0v9.8a4 4 0 1 0 4 0Z"/><path d="M12 9v8"/>"#
        }
        Icon::Check => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
        Icon::Close => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
        Icon::Gear => concat!(
            r#"<circle cx="12" cy="12" r="3"/>"#,
            r#"<path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1"#,
            r#"M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/>"#,
        ),
        Icon::Play => r#"<path d="M8 5.5v13l10.5-6.500Z" fill="currentColor"/>"#,
        Icon::Pause => r#"<path d="M9 5.500v13M15 5.500v13" stroke-width="2.600"/>"#,
        Icon::Stop => {
            r#"<rect x="6.500" y="6.500" width="11" height="11" rx="1.500" fill="currentColor"/>"#
        }
        Icon::Previous => {
            r#"<path d="M18 6v12l-8.500-6Z" fill="currentColor"/><path d="M6.500 6v12"/>"#
        }
        Icon::Next => r#"<path d="M6 6v12l8.500-6Z" fill="currentColor"/><path d="M17.500 6v12"/>"#,
        Icon::Power => r#"<path d="M12 3.500v8"/><path d="M7.050 6.500a7 7 0 1 0 9.900 0"/>"#,
        Icon::Volume => concat!(
            r#"<path d="M4 9.500h3l4.500-4v13l-4.500-4H4Z"/>"#,
            r#"<path d="M15.500 9a4 4 0 0 1 0 6M18 6.500a7.500 7.500 0 0 1 0 11"/>"#,
        ),
        Icon::Muted => {
            r#"<path d="M4 9.500h3l4.500-4v13l-4.500-4H4Z"/><path d="m16 9.500 5 5M21 9.500l-5 5"/>"#
        }
        Icon::Cast => concat!(
            r#"<path d="M3 8V6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-6"/>"#,
            r#"<path d="M3 12a8 8 0 0 1 8 8M3 16a4 4 0 0 1 4 4M3 20h.01"/>"#,
        ),
    }
}

pub fn icon(icon: Icon) -> AnyView {
    view! {
        <svg
            class="icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            inner_html=drawing(icon)
        ></svg>
    }
    .into_any()
}

/// An entity's own icon: what kind of thing it is, beside its name. The same one an
/// extension's page draws for it (`irori_ui_kit::entity_icon`).
pub fn entity(capabilities: &irori_types::Capabilities) -> AnyView {
    view! {
        <svg
            class="icon entity-icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            inner_html=irori_ui_kit::entity_icon::drawing(capabilities)
        ></svg>
    }
    .into_any()
}

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
    // The kinds of device on the Floorplan.
    Bulb,
    Tv,
    Speaker,
    Plug,
    /// Someone a radar can see.
    Person,
    Motion,
    Button,
    Air,
    Lock,
    Unlocked,
    Bell,
    Thermostat,
    /// Something that has stopped answering: a signal, struck through.
    NoSignal,
    /// Anything that isn't one of the others.
    Dot,
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
        Icon::Bulb => concat!(
            r#"<path d="M9 17.5h6M10 20.5h4"/>"#,
            r#"<path d="M8.5 14.5a6 6 0 1 1 7 0c-.6.5-1 1.200-1 2v1h-5v-1c0-.800-.4-1.500-1-2Z"/>"#,
        ),
        Icon::Tv => {
            r#"<rect x="3" y="5" width="18" height="12" rx="1.5"/><path d="M8.5 20.5h7M12 17v3.500"/>"#
        }
        Icon::Speaker => concat!(
            r#"<rect x="6.5" y="3" width="11" height="18" rx="2"/>"#,
            r#"<circle cx="12" cy="14.5" r="2.800"/><path d="M12 7.500h.01"/>"#,
        ),
        Icon::Plug => concat!(
            r#"<path d="M9 3v5M15 3v5M12 17v4"/>"#,
            r#"<path d="M6.5 8h11v3.500a5.500 5.500 0 0 1-11 0Z"/>"#,
        ),
        Icon::Person => {
            r#"<circle cx="12" cy="7" r="3.200"/><path d="M5.5 20.500a6.500 6.500 0 0 1 13 0"/>"#
        }
        Icon::Motion => concat!(
            r#"<circle cx="12" cy="12" r="1.600" fill="currentColor"/>"#,
            r#"<path d="M8.300 8.300a5.200 5.200 0 0 0 0 7.400M15.700 8.300a5.200 5.200 0 0 1 0 7.400"/>"#,
            r#"<path d="M5.500 5.500a9.200 9.200 0 0 0 0 13M18.500 5.500a9.200 9.200 0 0 1 0 13"/>"#,
        ),
        Icon::Button => {
            r#"<circle cx="12" cy="12" r="8.500"/><circle cx="12" cy="12" r="3.500" fill="currentColor"/>"#
        }
        Icon::Air => concat!(
            r#"<path d="M3 9h9.500a2.500 2.500 0 1 0-2.400-3.200"/>"#,
            r#"<path d="M3 13h14.500a2.800 2.800 0 1 1-2.700 3.600"/><path d="M3 17h7"/>"#,
        ),
        Icon::Lock => {
            r#"<rect x="5" y="10.500" width="14" height="10" rx="2"/><path d="M8 10.500V7.500a4 4 0 0 1 8 0v3"/>"#
        }
        Icon::Unlocked => {
            r#"<rect x="5" y="10.500" width="14" height="10" rx="2"/><path d="M8 10.500V7.500a4 4 0 0 1 7.700-1.500"/>"#
        }
        Icon::Bell => concat!(
            r#"<path d="M6 16.500V11a6 6 0 0 1 12 0v5.500l1.500 2h-15Z"/>"#,
            r#"<path d="M10.200 21a2 2 0 0 0 3.600 0M12 3v2"/>"#,
        ),
        Icon::Thermostat => concat!(
            r#"<circle cx="12" cy="12" r="8.500"/><path d="M12 12l3.200-3.200"/>"#,
            r#"<path d="M12 5.500v1.200M5.500 12h1.200M17.300 12h1.200"/>"#,
        ),
        // The stripe is the stylesheet's to colour (`.icon .stripe`): it is the one part of
        // any icon that isn't the colour of the text around it.
        Icon::NoSignal => concat!(
            r#"<path d="M12 18.500h.01"/><path d="M8.500 15a5 5 0 0 1 7 0"/>"#,
            r#"<path d="M5.500 12a9.200 9.200 0 0 1 13 0"/><path class="stripe" d="M5 20 19 4"/>"#,
        ),
        Icon::Dot => r#"<circle cx="12" cy="12" r="3.500" fill="currentColor"/>"#,
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

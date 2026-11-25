//! How a notification looks when `notifications.style` is `rich`.
//!
//! One table, total over [`Severity`], so a new severity cannot be added
//! without saying how it looks everywhere. Each notifier reads the columns it
//! can show: the freedesktop service takes urgency and category, macOS takes
//! a subtitle and a sound, and the title carries the glyph on both. `plain`,
//! the default, uses none of it and is what notifications looked like before.

use super::Severity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Leads the title, so the kind of news reads before the words do
    pub glyph: &'static str,
    /// macOS subtitle
    pub label: &'static str,
    /// freedesktop `urgency` hint: 0 low, 1 normal, 2 critical
    pub urgency: u8,
    /// freedesktop `category` hint, from the specification's network family
    pub category: &'static str,
    /// macOS sound, by the name in `/System/Library/Sounds`
    pub sound: &'static str,
}

pub fn of(severity: Severity) -> Style {
    match severity {
        Severity::Failure => Style {
            glyph: "\u{2716}",
            label: "Failed",
            urgency: 2,
            category: "network.error",
            sound: "Basso",
        },
        Severity::Warning => Style {
            glyph: "\u{26A0}",
            label: "Unhealthy",
            urgency: 1,
            category: "network.disconnected",
            sound: "Sosumi",
        },
        Severity::Recovered => Style {
            glyph: "\u{2714}",
            label: "Recovered",
            urgency: 0,
            category: "network.connected",
            sound: "Glass",
        },
        Severity::Info => Style {
            glyph: "\u{25CF}",
            label: "Started",
            urgency: 0,
            category: "network.connected",
            sound: "Pop",
        },
    }
}

/// The title a rich notification carries
pub fn title(style: &Style, program: &str) -> String {
    format!("{} {program}", style.glyph)
}

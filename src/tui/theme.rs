//! The only place in the TUI that names a color.
//!
//! Every value here is one of the terminal's own sixteen, never an `Rgb`. An
//! RGB palette looks identical in every terminal and is therefore wrong in
//! someone's: it ignores the theme the user chose, and a shade picked against a
//! dark background is unreadable on a light one. Naming the ANSI color instead
//! hands that decision back to the terminal, which is the only thing that knows
//! what the background is.
//!
//! A color means one thing. `ACCENT` marks what gtunnel measured or derived,
//! `MUTED` is for labels that must not compete with their values, and the three
//! status colors answer one question each.

use ratatui::style::Color;

/// What gtunnel measured or derived: a mode, a chart, a figure worth reading.
pub const ACCENT: Color = Color::Cyan;

/// A field label. It names the value beside it and must lose to it.
pub const MUTED: Color = Color::DarkGray;

/// Running, healthy, done.
pub const OK: Color = Color::Green;

/// Worth a look, but nothing is broken yet.
pub const WARN: Color = Color::Yellow;

/// Broken: a failed tunnel, an operation that did not happen.
pub const BAD: Color = Color::Red;

/// A setting that is on, as against a state that is good. Auto-start is a
/// choice the user made, not a verdict, so it is not `OK`.
pub const FLAG: Color = Color::Blue;

/// The selected row. Two steps off the background rather than a block of color,
/// so the row the cursor is on reads as emphasis and not as a warning.
pub const SELECTED_BG: Color = Color::Indexed(236);

/// Text that sits on top of `ACCENT`.
pub const ON_ACCENT: Color = Color::Black;

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
//!
//! The logo is the one exception, and it is deliberate. Its hues are not a
//! judgement about state that the terminal's theme should get to reinterpret —
//! they are the mark itself, and a mark that changes colour between terminals is
//! a different mark. So the `LOGO_*` constants name exact hues and accept that a
//! sixteen-colour terminal will approximate them. Nothing else may follow them:
//! if a value answers a question about what gtunnel found, it stays ANSI.

use ratatui::style::Color;

/// What gtunnel measured or derived: a mode, a chart, a figure worth reading.
pub const ACCENT: Color = Color::Cyan;

/// Furniture: a rule, a divider, a pane's name. It frames what the user came
/// to read and must recede behind all of it.
pub const MUTED: Color = Color::DarkGray;

/// A field label, and the word beside a key in the footer. It names the thing
/// next to it, so it loses to that value but still has to be read on its own —
/// which is why it is one step brighter than the furniture around it.
pub const LABEL: Color = Color::Gray;

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

/// The line the cloud rests on, and the two cells at each end of the mark.
pub const LOGO_LINE: Color = Color::Rgb(0x4D, 0xD0, 0xE1);

/// The mark's first band, reading left to right.
pub const LOGO_BLUE: Color = Color::Rgb(0x42, 0x85, 0xF4);

/// The second band.
pub const LOGO_RED: Color = Color::Rgb(0xEA, 0x43, 0x35);

/// The third band.
pub const LOGO_YELLOW: Color = Color::Rgb(0xFB, 0xBC, 0x04);

/// The fourth band.
pub const LOGO_GREEN: Color = Color::Rgb(0x34, 0xA8, 0x53);

//! Styling constants and helpers. Papermend prints through `anstream`, which strips colour for
//! non-terminals and honours `NO_COLOR` / `CLICOLOR`.

use clap::builder::styling::{AnsiColor, Effects, Style, Styles};
use std::fmt::Display;

pub const CLAP_STYLE: Styles = Styles::styled()
    .header(GOOD)
    .usage(GOOD)
    .literal(KEY)
    .placeholder(AnsiColor::Cyan.on_default())
    .error(BAD)
    .valid(KEY)
    .invalid(WARN);

pub const GOOD: Style = AnsiColor::BrightGreen.on_default().effects(Effects::BOLD);
pub const BAD: Style = AnsiColor::BrightRed.on_default().effects(Effects::BOLD);
pub const WARN: Style = AnsiColor::BrightYellow.on_default().effects(Effects::BOLD);
pub const KEY: Style = AnsiColor::BrightCyan.on_default().effects(Effects::BOLD);

/// Coloured, key-value line.
pub fn line(style: Style, label: &str, text: impl Display) -> String {
    format!("{style}{label:<10}{style:#} {text}")
}

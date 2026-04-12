// Low-level terminal primitives used by the interactive patcher.
//
// Derived from fastmod (Copyright Meta Platforms, Inc. and affiliates),
// used under the Apache License, Version 2.0. See LICENSE and NOTICE
// at the repo root for details.

use std::io;

use crossterm::ExecutableCommand as _;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;

pub(super) enum Color {
    Red,
    Green,
}

impl Color {
    fn to_crossterm_color(&self) -> crossterm::style::Color {
        match self {
            Self::Red => crossterm::style::Color::DarkRed,
            Self::Green => crossterm::style::Color::DarkGreen,
        }
    }
}

pub(super) fn clear() {
    let mut stdout = io::stdout();
    if stdout.execute(Clear(ClearType::All)).is_err() {
        print!("{}", "\n".repeat(8));
    }
    drop(stdout.execute(crossterm::cursor::MoveTo(0, 0)));
}

pub(super) fn hide_cursor() {
    drop(io::stdout().execute(crossterm::cursor::Hide));
}

pub(super) fn show_cursor() {
    drop(io::stdout().execute(crossterm::cursor::Show));
}

pub(super) fn fg(color: Color) {
    drop(io::stdout().execute(SetForegroundColor(color.to_crossterm_color())));
}

pub(super) fn reset() {
    drop(io::stdout().execute(SetForegroundColor(crossterm::style::Color::Reset)));
}

pub(super) fn size() -> Option<(usize, usize)> {
    crossterm::terminal::size()
        .ok()
        .map(|(w, h)| (w as usize, h as usize))
}

// Low-level terminal primitives used by the interactive patcher.
//
// Derived from fastmod (Copyright Meta Platforms, Inc. and affiliates),
// used under the Apache License, Version 2.0. See LICENSE and NOTICE
// at the repo root for details.

use std::io;

use crossterm::ExecutableCommand as _;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;

pub(super) fn clear() {
    let mut stdout = io::stdout();
    if stdout.execute(Clear(ClearType::All)).is_err() {
        print!("{}", "\n".repeat(8));
    }
    drop(stdout.execute(crossterm::cursor::MoveTo(0, 0)));
}

pub(super) struct HiddenCursor;

impl HiddenCursor {
    pub(super) fn new() -> Self {
        drop(io::stdout().execute(crossterm::cursor::Hide));
        Self
    }
}

impl Drop for HiddenCursor {
    fn drop(&mut self) {
        drop(io::stdout().execute(crossterm::cursor::Show));
    }
}

pub(super) fn size() -> Option<(usize, usize)> {
    crossterm::terminal::size()
        .ok()
        .map(|(w, h)| (w as usize, h as usize))
}

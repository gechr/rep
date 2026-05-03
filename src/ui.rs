#[derive(Clone, Copy)]
pub(crate) enum Color {
    Blue,
    Dim,
    Green,
    Grey,
    Magenta,
    Red,
    White,
    Yellow,
}

impl Color {
    const fn ansi(self) -> &'static str {
        match self {
            Self::Blue => "\x1b[34m",
            Self::Dim => "\x1b[2m",
            Self::Green => "\x1b[32m",
            Self::Grey => "\x1b[38;5;248m",
            Self::Magenta => "\x1b[35m",
            Self::Red => "\x1b[31m",
            Self::White => "\x1b[37m",
            Self::Yellow => "\x1b[33m",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Styles {
    enabled: bool,
}

impl Styles {
    pub(crate) const PLAIN: Self = Self { enabled: false };

    pub(crate) const fn ansi() -> Self {
        Self { enabled: true }
    }

    pub(crate) fn when(enabled: bool) -> Self {
        if enabled && !no_color() {
            Self::ansi()
        } else {
            Self::PLAIN
        }
    }

    pub(crate) const fn is_plain(self) -> bool {
        !self.enabled
    }

    pub(crate) const fn fg(self, color: Color) -> &'static str {
        if self.enabled { color.ansi() } else { "" }
    }

    pub(crate) const fn bold(self) -> &'static str {
        if self.enabled { "\x1b[1m" } else { "" }
    }

    pub(crate) const fn dim(self) -> &'static str {
        if self.enabled { "\x1b[2m" } else { "" }
    }

    pub(crate) const fn underline(self) -> &'static str {
        if self.enabled { "\x1b[4m" } else { "" }
    }

    pub(crate) const fn reset(self) -> &'static str {
        if self.enabled { "\x1b[m" } else { "" }
    }

    pub(crate) fn paint(self, color: Color, text: impl AsRef<str>) -> String {
        format!("{}{}{}", self.fg(color), text.as_ref(), self.reset())
    }

    pub(crate) fn bold_paint(self, color: Color, text: impl AsRef<str>) -> String {
        format!(
            "{}{}{}{}",
            self.fg(color),
            self.bold(),
            text.as_ref(),
            self.reset()
        )
    }

    pub(crate) fn print_fg(self, color: Color) {
        print!("{}", self.fg(color));
    }

    pub(crate) fn print_reset(self) {
        print!("{}", self.reset());
    }
}

/// <https://no-color.org>
pub(crate) fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

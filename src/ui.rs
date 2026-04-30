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
    fn ansi(self) -> &'static str {
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

    fn ansi_bg(self) -> &'static str {
        match self {
            Self::Blue => "\x1b[44m",
            Self::Dim => "\x1b[2m",
            Self::Green => "\x1b[48;2;0;255;0m",
            Self::Grey => "\x1b[48;5;248m",
            Self::Magenta => "\x1b[45m",
            Self::Red => "\x1b[48;2;255;0;0m",
            Self::White => "\x1b[47m",
            Self::Yellow => "\x1b[43m",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Styles {
    enabled: bool,
}

impl Styles {
    pub(crate) const PLAIN: Self = Self { enabled: false };

    pub(crate) fn ansi() -> Self {
        Self { enabled: true }
    }

    pub(crate) fn when(enabled: bool) -> Self {
        if enabled { Self::ansi() } else { Self::PLAIN }
    }

    pub(crate) fn is_plain(self) -> bool {
        !self.enabled
    }

    pub(crate) fn fg(self, color: Color) -> &'static str {
        if self.enabled { color.ansi() } else { "" }
    }

    pub(crate) fn bold(self) -> &'static str {
        if self.enabled { "\x1b[1m" } else { "" }
    }

    pub(crate) fn bold_off(self) -> &'static str {
        if self.enabled { "\x1b[22m" } else { "" }
    }

    pub(crate) fn reset(self) -> &'static str {
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

    pub(crate) fn dim_bg(self, color: Color) -> &'static str {
        if !self.enabled {
            return "";
        }
        match color {
            Color::Red => "\x1b[48;5;52m",
            Color::Green => "\x1b[48;5;22m",
            _ => color.ansi_bg(),
        }
    }

    pub(crate) fn print_fg(self, color: Color) {
        print!("{}", self.fg(color));
    }

    pub(crate) fn print_reset(self) {
        print!("{}", self.reset());
    }
}

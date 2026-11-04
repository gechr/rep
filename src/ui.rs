#[derive(Clone, Copy, PartialEq, Eq)]
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

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy)]
pub(crate) struct Styles<'a> {
    enabled: bool,
    pub(crate) theme: &'a crate::theme::Theme,
}

impl Styles<'_> {
    pub(crate) const PLAIN: Self = Self {
        enabled: false,
        theme: &crate::theme::Theme::DEFAULT,
    };

    pub(crate) const fn ansi() -> Self {
        Self {
            enabled: true,
            theme: &crate::theme::Theme::DEFAULT,
        }
    }

    pub(crate) const fn resolve(choice: ColorChoice, terminal: bool, no_color: bool) -> Self {
        match choice {
            ColorChoice::Always => Self::ansi(),
            ColorChoice::Never => Self::PLAIN,
            ColorChoice::Auto => {
                if terminal && !no_color {
                    Self::ansi()
                } else {
                    Self::PLAIN
                }
            }
        }
    }

    pub(crate) const fn with_theme(self, theme: &crate::theme::Theme) -> Styles<'_> {
        Styles {
            enabled: self.enabled,
            theme,
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
}

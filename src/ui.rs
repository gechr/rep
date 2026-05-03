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

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR_CHOICE: std::sync::OnceLock<ColorChoice> = std::sync::OnceLock::new();

/// Lock in the user's `--color` selection. First write wins; subsequent calls
/// are silently ignored, which keeps tests and the rc-args path safe.
pub(crate) fn set_color_choice(choice: ColorChoice) {
    let _ = COLOR_CHOICE.set(choice);
}

pub(crate) fn color_choice() -> ColorChoice {
    COLOR_CHOICE.get().copied().unwrap_or_default()
}

/// <https://no-color.org>
pub(crate) fn no_color() -> bool {
    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
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
        match color_choice() {
            ColorChoice::Always => Self::ansi(),
            ColorChoice::Never => Self::PLAIN,
            ColorChoice::Auto => {
                if enabled && !no_color() {
                    Self::ansi()
                } else {
                    Self::PLAIN
                }
            }
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

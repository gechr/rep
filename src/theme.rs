//! Configurable color/attribute palette for diff output.
//!
//! Style strings follow the git/delta convention: whitespace-separated tokens,
//! case-insensitive. The first color token sets the foreground, the second sets
//! the background; attributes (`bold`, `dim`, `underline`, …) intermix freely.
//! `default` skips a color slot, e.g. `default red` is "background-only".

use std::fmt::Write as _;
use std::sync::OnceLock;

use crossterm::style::Color;

use crate::ui::Styles;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct StyleSpec {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strike: bool,
}

impl StyleSpec {
    /// SGR opening sequence for this spec. Empty when `styles` is plain so the
    /// uncolored output path stays untouched.
    pub fn open(self, styles: Styles) -> String {
        let mut out = String::new();
        self.open_into(&mut out, styles);
        out
    }

    /// Push the SGR opening sequence directly into `out`. The hot per-line
    /// caller uses this to skip the owned-`String` return path entirely.
    ///
    /// All parameters are folded into a single CSI sequence
    /// (`\x1b[31;2m`, not `\x1b[31m\x1b[2m`): every escape introducer is a
    /// separate dispatch in the terminal's parser, and diff output emits
    /// these once or twice per line, so fewer sequences directly cuts the
    /// terminal-side cost of rendering large diffs.
    pub fn open_into(self, out: &mut String, styles: Styles) {
        if styles.is_plain() {
            return;
        }
        let mut any = false;
        if let Some(c) = self.fg {
            sgr_color_params(out, c, false, &mut any);
        }
        if let Some(c) = self.bg {
            sgr_color_params(out, c, true, &mut any);
        }
        for (param, on) in [
            ('1', self.bold),
            ('2', self.dim),
            ('3', self.italic),
            ('4', self.underline),
            ('5', self.blink),
            ('7', self.reverse),
            ('8', self.hidden),
            ('9', self.strike),
        ] {
            if on {
                push_sgr_separator(out, &mut any);
                out.push(param);
            }
        }
        if any {
            out.push('m');
        }
    }

    /// Drop the underline attribute. Used for whole-line emission, where
    /// underlining a full multi-line block reads as visual noise.
    pub const fn without_underline(mut self) -> Self {
        self.underline = false;
        self
    }
}

/// Open the CSI on the first parameter; separate subsequent ones with `;` so
/// the whole spec lands in a single SGR sequence. The caller appends the
/// final `m` once any parameter was written.
fn push_sgr_separator(out: &mut String, any: &mut bool) {
    if *any {
        out.push(';');
    } else {
        out.push_str("\x1b[");
        *any = true;
    }
}

/// Push the SGR parameters for `color` into `out`: `30`-`37` for the standard
/// palette, `90`-`97` for the bright palette, `38;5;N` for indexed, `38;2;R;G;B`
/// for truecolor (and `+10` for backgrounds). The separator is pushed only
/// when the color actually produces parameters, so `Color::Reset` (and any
/// unknown variant) cannot leave an empty - implicitly `0` - SGR parameter.
fn sgr_color_params(out: &mut String, color: Color, bg: bool, any: &mut bool) {
    if let Some(s) = static_sgr_color(color, bg) {
        push_sgr_separator(out, any);
        out.push_str(s);
        return;
    }
    let extended: u32 = if bg { 48 } else { 38 };
    match color {
        Color::Rgb { r, g, b } => {
            push_sgr_separator(out, any);
            let _ = write!(out, "{extended};2;{r};{g};{b}");
        }
        Color::AnsiValue(n) => {
            push_sgr_separator(out, any);
            let _ = write!(out, "{extended};5;{n}");
        }
        _ => {}
    }
}

const fn static_sgr_color(color: Color, bg: bool) -> Option<&'static str> {
    Some(match (color, bg) {
        (Color::Black, false) => "30",
        (Color::DarkRed, false) => "31",
        (Color::DarkGreen, false) => "32",
        (Color::DarkYellow, false) => "33",
        (Color::DarkBlue, false) => "34",
        (Color::DarkMagenta, false) => "35",
        (Color::DarkCyan, false) => "36",
        (Color::Grey, false) => "37",
        (Color::DarkGrey, false) => "90",
        (Color::Red, false) => "91",
        (Color::Green, false) => "92",
        (Color::Yellow, false) => "93",
        (Color::Blue, false) => "94",
        (Color::Magenta, false) => "95",
        (Color::Cyan, false) => "96",
        (Color::White, false) => "97",
        (Color::Black, true) => "40",
        (Color::DarkRed, true) => "41",
        (Color::DarkGreen, true) => "42",
        (Color::DarkYellow, true) => "43",
        (Color::DarkBlue, true) => "44",
        (Color::DarkMagenta, true) => "45",
        (Color::DarkCyan, true) => "46",
        (Color::Grey, true) => "47",
        (Color::DarkGrey, true) => "100",
        (Color::Red, true) => "101",
        (Color::Green, true) => "102",
        (Color::Yellow, true) => "103",
        (Color::Blue, true) => "104",
        (Color::Magenta, true) => "105",
        (Color::Cyan, true) => "106",
        (Color::White, true) => "107",
        _ => return None,
    })
}

/// Resolved palette and marker config. `marker_*` are emitted verbatim when
/// `Some`; when `None`, `+`/`-` appear only in uncolored output and are omitted
/// otherwise.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub style_added: StyleSpec,
    pub style_removed: StyleSpec,
    pub style_line_added: StyleSpec,
    pub style_line_removed: StyleSpec,
    pub marker_added: Option<String>,
    pub marker_removed: Option<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            style_added: StyleSpec {
                fg: Some(Color::DarkGreen),
                underline: true,
                ..Default::default()
            },
            style_removed: StyleSpec {
                fg: Some(Color::DarkRed),
                underline: true,
                ..Default::default()
            },
            style_line_added: StyleSpec {
                fg: Some(Color::DarkGreen),
                dim: true,
                ..Default::default()
            },
            style_line_removed: StyleSpec {
                fg: Some(Color::DarkRed),
                dim: true,
                ..Default::default()
            },
            marker_added: None,
            marker_removed: None,
        }
    }
}

/// Bundle of CLI-derived overrides; any `None` keeps the default for that slot.
#[derive(Default)]
pub(crate) struct Overrides<'a> {
    pub style_added: Option<&'a str>,
    pub style_removed: Option<&'a str>,
    pub style_line_added: Option<&'a str>,
    pub style_line_removed: Option<&'a str>,
    pub marker_added: Option<String>,
    pub marker_removed: Option<String>,
}

impl Theme {
    /// Build a theme by parsing user overrides on top of the defaults. Returns
    /// the first parse error so the caller can surface it as a CLI failure.
    pub fn from_overrides(o: Overrides<'_>) -> Result<Self, String> {
        let mut t = Self::default();
        if let Some(s) = o.style_added {
            t.style_added = parse(s)?;
        }
        if let Some(s) = o.style_removed {
            t.style_removed = parse(s)?;
        }
        if let Some(s) = o.style_line_added {
            t.style_line_added = parse(s)?;
        }
        if let Some(s) = o.style_line_removed {
            t.style_line_removed = parse(s)?;
        }
        t.marker_added = o.marker_added;
        t.marker_removed = o.marker_removed;
        Ok(t)
    }

    /// Resolve the marker for a side. An explicit value is emitted verbatim in
    /// both colored and plain modes. With no explicit value, `+`/`-` appears
    /// only in plain output (where colors aren't carrying the signal).
    pub fn marker_for(&self, side: Side, plain: bool) -> &str {
        let (explicit, fallback) = match side {
            Side::Added => (self.marker_added.as_deref(), "+"),
            Side::Removed => (self.marker_removed.as_deref(), "-"),
        };
        match (explicit, plain) {
            (Some(s), _) => s,
            (None, true) => fallback,
            (None, false) => "",
        }
    }

    pub const fn has_explicit_marker(&self, side: Side) -> bool {
        match side {
            Side::Added => self.marker_added.is_some(),
            Side::Removed => self.marker_removed.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Added,
    Removed,
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Lock in the resolved theme; first write wins. Subsequent calls are silent
/// no-ops, so in-process integration tests cannot install per-test themes once
/// any prior test has triggered initialization.
pub(crate) fn set_theme(theme: Theme) {
    drop(THEME.set(theme));
}

pub(crate) fn theme() -> &'static Theme {
    THEME.get_or_init(Theme::default)
}

/// Parse a git-style style string into a [`StyleSpec`].
pub(crate) fn parse(s: &str) -> Result<StyleSpec, String> {
    let mut spec = StyleSpec::default();
    let mut color_slot: u8 = 0;
    for raw in s.split_whitespace() {
        let token = raw.to_ascii_lowercase();
        match token.as_str() {
            "bold" => spec.bold = true,
            "dim" | "dimmed" => spec.dim = true,
            "italic" | "italics" => spec.italic = true,
            "underline" | "underlined" | "ul" => spec.underline = true,
            "blink" => spec.blink = true,
            "reverse" | "reversed" | "invert" | "inverted" => spec.reverse = true,
            "hidden" => spec.hidden = true,
            "strike" | "strikethrough" | "strikethru" => spec.strike = true,
            _ => {
                let color = parse_color(&token)
                    .ok_or_else(|| format!("unknown color or attribute: {raw:?}"))?;
                match color_slot {
                    0 => {
                        spec.fg = color;
                        color_slot = 1;
                    }
                    1 => {
                        spec.bg = color;
                        color_slot = 2;
                    }
                    _ => return Err(format!("too many colors in style string {s:?}")),
                }
            }
        }
    }
    Ok(spec)
}

/// Bare color names follow git/delta convention: SGR 30-37 (the standard
/// terminal palette), which crossterm spells as `Dark*`. `bright-*` (or the
/// joined `bright*`) addresses the SGR 90-97 vivid variants.
fn parse_color(token: &str) -> Option<Option<Color>> {
    match token {
        "default" | "normal" => Some(None),
        "black" => Some(Some(Color::Black)),
        "red" => Some(Some(Color::DarkRed)),
        "green" => Some(Some(Color::DarkGreen)),
        "yellow" => Some(Some(Color::DarkYellow)),
        "blue" => Some(Some(Color::DarkBlue)),
        "magenta" | "purple" => Some(Some(Color::DarkMagenta)),
        "cyan" => Some(Some(Color::DarkCyan)),
        "white" => Some(Some(Color::Grey)),
        "grey" | "gray" => Some(Some(Color::DarkGrey)),
        "bright-black" | "brightblack" => Some(Some(Color::DarkGrey)),
        "bright-red" | "brightred" => Some(Some(Color::Red)),
        "bright-green" | "brightgreen" => Some(Some(Color::Green)),
        "bright-yellow" | "brightyellow" => Some(Some(Color::Yellow)),
        "bright-blue" | "brightblue" => Some(Some(Color::Blue)),
        "bright-magenta" | "brightmagenta" | "bright-purple" | "brightpurple" => {
            Some(Some(Color::Magenta))
        }
        "bright-cyan" | "brightcyan" => Some(Some(Color::Cyan)),
        "bright-white" | "brightwhite" => Some(Some(Color::White)),
        s if s.starts_with('#') => parse_hex(s).map(Some),
        _ => None,
    }
}

/// Accept `#rrggbb` and the `#rgb` shorthand (each digit duplicated).
fn parse_hex(s: &str) -> Option<Color> {
    let hex = &s[1..];
    if !hex.is_ascii() {
        return None;
    }
    let (r, g, b) = match hex.len() {
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            (r * 17, g * 17, b * 17)
        }
        _ => return None,
    };
    Some(Color::Rgb { r, g, b })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_color_only() {
        assert_eq!(
            parse("red").unwrap(),
            StyleSpec {
                fg: Some(Color::DarkRed),
                ..Default::default()
            }
        );
    }

    #[test]
    fn parses_two_colors_positionally() {
        assert_eq!(
            parse("black red").unwrap(),
            StyleSpec {
                fg: Some(Color::Black),
                bg: Some(Color::DarkRed),
                ..Default::default()
            }
        );
    }

    #[test]
    fn parses_attributes_anywhere() {
        assert_eq!(
            parse("bold red ul").unwrap(),
            StyleSpec {
                fg: Some(Color::DarkRed),
                bold: true,
                underline: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn bright_addresses_the_vivid_palette() {
        assert_eq!(
            parse("bright-red").unwrap(),
            StyleSpec {
                fg: Some(Color::Red),
                ..Default::default()
            }
        );
    }

    #[test]
    fn accepts_attribute_aliases() {
        let s = parse("dimmed underlined reversed").unwrap();
        assert!(s.dim);
        assert!(s.underline);
        assert!(s.reverse);
    }

    #[test]
    fn default_skips_color_slot() {
        assert_eq!(
            parse("default red").unwrap(),
            StyleSpec {
                fg: None,
                bg: Some(Color::DarkRed),
                ..Default::default()
            }
        );
    }

    #[test]
    fn normal_aliases_default() {
        assert_eq!(parse("normal red").unwrap(), parse("default red").unwrap());
    }

    #[test]
    fn parses_six_digit_hex() {
        let s = parse("#aabbcc").unwrap();
        assert_eq!(
            s.fg,
            Some(Color::Rgb {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc
            })
        );
    }

    #[test]
    fn parses_three_digit_hex() {
        let s = parse("#abc").unwrap();
        assert_eq!(
            s.fg,
            Some(Color::Rgb {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc
            })
        );
    }

    #[test]
    fn rejects_unknown_token() {
        assert!(parse("boold").is_err());
    }

    #[test]
    fn rejects_non_ascii_hex() {
        // `é` is two bytes in UTF-8; without the ASCII guard, byte-slicing
        // the 3-char shorthand path lands mid-codepoint and panics.
        assert!(parse("#aé").is_err());
    }

    #[test]
    fn rejects_non_hex_digits() {
        assert!(parse("#xyz").is_err());
    }

    #[test]
    fn rejects_hex_of_invalid_length() {
        assert!(parse("#1234567").is_err());
        assert!(parse("#12").is_err());
        assert!(parse("#").is_err());
    }

    #[test]
    fn rejects_third_color() {
        assert!(parse("red green blue").is_err());
    }

    #[test]
    fn without_underline_clears_only_underline() {
        assert_eq!(
            parse("red ul bold").unwrap().without_underline(),
            StyleSpec {
                fg: Some(Color::DarkRed),
                bold: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn open_is_empty_when_plain() {
        let spec = parse("red bold").unwrap();
        assert_eq!(spec.open(Styles::PLAIN), "");
    }

    #[test]
    fn open_emits_color_then_attributes() {
        assert_eq!(
            parse("red bold").unwrap().open(Styles::ansi()),
            "\x1b[31;1m"
        );
    }

    #[test]
    fn open_emits_bg_only() {
        assert_eq!(
            parse("default red").unwrap().open(Styles::ansi()),
            "\x1b[41m"
        );
    }

    #[test]
    fn open_emits_truecolor_fg() {
        assert_eq!(
            parse("#aabbcc").unwrap().open(Styles::ansi()),
            "\x1b[38;2;170;187;204m"
        );
    }

    #[test]
    fn open_emits_truecolor_bg() {
        assert_eq!(
            parse("default #303030").unwrap().open(Styles::ansi()),
            "\x1b[48;2;48;48;48m"
        );
    }

    #[test]
    fn open_emits_attribute_only() {
        assert_eq!(parse("bold").unwrap().open(Styles::ansi()), "\x1b[1m");
    }

    #[test]
    fn open_emits_fg_and_bg() {
        assert_eq!(
            parse("white red").unwrap().open(Styles::ansi()),
            "\x1b[37;41m"
        );
    }

    #[test]
    fn marker_for_added_truth_table() {
        let mut t = Theme::default();
        assert!(!t.has_explicit_marker(Side::Added));
        assert_eq!(t.marker_for(Side::Added, true), "+");
        assert_eq!(t.marker_for(Side::Added, false), "");

        t.marker_added = Some(String::new());
        assert!(t.has_explicit_marker(Side::Added));
        assert_eq!(t.marker_for(Side::Added, true), "");
        assert_eq!(t.marker_for(Side::Added, false), "");

        t.marker_added = Some(" ".into());
        assert!(t.has_explicit_marker(Side::Added));
        assert_eq!(t.marker_for(Side::Added, true), " ");
        assert_eq!(t.marker_for(Side::Added, false), " ");

        t.marker_added = Some("▎".into());
        assert!(t.has_explicit_marker(Side::Added));
        assert_eq!(t.marker_for(Side::Added, true), "▎");
        assert_eq!(t.marker_for(Side::Added, false), "▎");
    }

    #[test]
    fn marker_for_removed_truth_table() {
        let mut t = Theme::default();
        assert!(!t.has_explicit_marker(Side::Removed));
        assert_eq!(t.marker_for(Side::Removed, true), "-");
        assert_eq!(t.marker_for(Side::Removed, false), "");

        t.marker_removed = Some(String::new());
        assert!(t.has_explicit_marker(Side::Removed));
        assert_eq!(t.marker_for(Side::Removed, true), "");
        assert_eq!(t.marker_for(Side::Removed, false), "");

        t.marker_removed = Some(">>".into());
        assert!(t.has_explicit_marker(Side::Removed));
        assert_eq!(t.marker_for(Side::Removed, true), ">>");
        assert_eq!(t.marker_for(Side::Removed, false), ">>");
    }
}

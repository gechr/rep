use std::path::PathBuf;

pub(crate) fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

pub(crate) fn hyperlink_path(path: &std::path::Path) -> String {
    // The working directory cannot change mid-run, so resolve it once
    // instead of issuing a `getcwd` syscall per matched file.
    static CWD: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        CWD.get_or_init(|| std::env::current_dir().ok())
            .as_ref()
            .map_or_else(|| path.to_path_buf(), |cwd| cwd.join(path))
    };
    // `join` keeps a relative path's `.` components, so collapse `.` and
    // redundant separators for a clean URL. `..` is left intact; it can't be
    // resolved without following symlinks.
    abs.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .to_string()
}

pub(crate) fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Resolves an alias or a literal format string to the format that
/// `hyperlink_url` consumes. `None` means "hyperlinks disabled" (the user
/// passed an empty string or the `none` alias).
pub(crate) fn hyperlink_format(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let resolved = match trimmed.to_ascii_lowercase().as_str() {
        "none" => return None,
        "cursor" => "cursor://file{path}:{line}:{column}",
        "file" | "default" => "file://{host}{path}",
        "grep+" => "grep+://{path}:{line}",
        "kitty" => "file://{host}{path}#{line}",
        "macvim" => "mvim://open?url=file://{path}&line={line}&column={column}",
        "textmate" => "txmt://open?url=file://{path}&line={line}&column={column}",
        "vscode" => "vscode://file{path}:{line}:{column}",
        "vscode-insiders" => "vscode-insiders://file{path}:{line}:{column}",
        "vscodium" => "vscodium://file{path}:{line}:{column}",
        _ => return Some(trimmed.to_string()),
    };
    Some(resolved.to_string())
}

pub(crate) fn hyperlink_format_uses_column(format: &str) -> bool {
    format.contains("{column}")
}

/// System hostname, cached. `None` if it can't be resolved or isn't UTF-8.
pub(crate) fn hostname() -> Option<&'static str> {
    static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOST.get_or_init(|| gethostname::gethostname().into_string().ok())
        .as_deref()
}

/// Percent-encodes a path per RFC 3986 §2.3 unreserved set, plus `/` and `:`
/// (preserved as path separators). Bytes >= 128 (UTF-8 continuations) pass
/// through unencoded.
pub(crate) fn percent_encode_path(s: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
            | b'/'
            | b':'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | 128.. => out.push(b),
            _ => {
                out.push(b'%');
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 0xF) as usize]);
            }
        }
    }
    String::from_utf8(out)
        .expect("UTF-8 by construction: input bytes preserved or replaced by ASCII")
}

#[derive(Clone, Copy)]
enum HyperlinkSeg<'a> {
    Lit(&'a str),
    Path,
    Host,
    Line,
    Column,
}

/// Pre-parsed hyperlink format. The format is scanned once for the
/// supported `{path}/{host}/{line}/{column}` placeholders and split into
/// a sequence of literal slices and placeholder slots. Each render is a
/// single linear pass over `segs` with one final `String` allocation.
#[derive(Clone)]
pub(crate) struct HyperlinkTemplate<'a> {
    segs: Vec<HyperlinkSeg<'a>>,
    has_path: bool,
    has_line_or_column: bool,
}

impl<'a> HyperlinkTemplate<'a> {
    pub(crate) fn parse(format: &'a str) -> Self {
        let mut segs: Vec<HyperlinkSeg<'a>> = Vec::new();
        let mut has_path = false;
        let mut has_line_or_column = false;
        let mut rest = format;
        while let Some(open) = rest.find('{') {
            let after_open = &rest[open + 1..];
            let Some(close_rel) = after_open.find('}') else {
                break;
            };
            let name = &after_open[..close_rel];
            let consumed_to = open + 1 + close_rel + 1;
            let seg = match name {
                "path" => {
                    has_path = true;
                    HyperlinkSeg::Path
                }
                "host" => HyperlinkSeg::Host,
                "line" => {
                    has_line_or_column = true;
                    HyperlinkSeg::Line
                }
                "column" => {
                    has_line_or_column = true;
                    HyperlinkSeg::Column
                }
                _ => {
                    segs.push(HyperlinkSeg::Lit(&rest[..consumed_to]));
                    rest = &rest[consumed_to..];
                    continue;
                }
            };
            if open > 0 {
                segs.push(HyperlinkSeg::Lit(&rest[..open]));
            }
            segs.push(seg);
            rest = &rest[consumed_to..];
        }
        if !rest.is_empty() {
            segs.push(HyperlinkSeg::Lit(rest));
        }
        Self {
            segs,
            has_path,
            has_line_or_column,
        }
    }

    pub(crate) const fn uses_path(&self) -> bool {
        self.has_path
    }

    /// True when the template varies per line (references `{line}` or
    /// `{column}`). When false, every line's link is identical to the file
    /// header link, so the diff renderer skips per-line OSC 8 entirely - one
    /// link per file instead of one per line.
    pub(crate) const fn links_lines(&self) -> bool {
        self.has_line_or_column
    }

    /// Render directly into `out`. `encoded_path` is the percent-encoded path;
    /// pass `""` when the template doesn't reference `{path}`. `line`/`column`
    /// of `0` render as `1`.
    pub(crate) fn render_into(
        &self,
        out: &mut String,
        encoded_path: &str,
        line: usize,
        column: usize,
    ) {
        for seg in &self.segs {
            match seg {
                HyperlinkSeg::Lit(s) => out.push_str(s),
                HyperlinkSeg::Path => out.push_str(encoded_path),
                HyperlinkSeg::Host => out.push_str(hostname().unwrap_or("")),
                HyperlinkSeg::Line => push_decimal(out, if line == 0 { 1 } else { line }),
                HyperlinkSeg::Column => push_decimal(out, if column == 0 { 1 } else { column }),
            }
        }
    }

    pub(crate) fn render(&self, encoded_path: &str, line: usize, column: usize) -> String {
        let mut out = String::with_capacity(encoded_path.len() + 32);
        self.render_into(&mut out, encoded_path, line, column);
        out
    }
}

/// Push `n` as decimal ASCII into `out` without going through `core::fmt`.
pub(crate) fn push_decimal(out: &mut String, mut n: usize) {
    if n == 0 {
        out.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.push_str(std::str::from_utf8(&buf[i..]).expect("digits are ASCII"));
}

#[cfg(test)]
pub(crate) fn hyperlink_url(format: &str, path: &str, line: usize, column: usize) -> String {
    let template = HyperlinkTemplate::parse(format);
    let encoded = if template.uses_path() {
        percent_encode_path(path)
    } else {
        String::new()
    };
    template.render(&encoded, line, column)
}

pub(crate) fn hyperlink_with_template(
    template: Option<&HyperlinkTemplate<'_>>,
    encoded_path: &str,
    line: usize,
    text: &str,
) -> String {
    template.map_or_else(
        || text.to_string(),
        |t| osc8(&t.render(encoded_path, line, 0), text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_display_path_strips_leading_dot_slash() {
        assert_eq!(
            display_path(std::path::Path::new("./src/main.rs")),
            "src/main.rs"
        );
    }

    #[test]
    fn test_display_path_preserves_plain_path() {
        assert_eq!(
            display_path(std::path::Path::new("src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(display_path(std::path::Path::new("/abs/path")), "/abs/path");
    }

    #[test]
    fn test_hyperlink_path_collapses_dot_components() {
        assert_eq!(
            hyperlink_path(std::path::Path::new("/abs/./tests/cli.rs")),
            "/abs/tests/cli.rs"
        );
    }

    #[test]
    fn test_hyperlink_url_expands_path_and_line() {
        assert_eq!(
            hyperlink_url("vscode://file{path}:{line}", "/tmp/a.txt", 42, 0),
            "vscode://file/tmp/a.txt:42"
        );
    }

    #[test]
    fn test_hyperlink_format_expands_presets() {
        assert_eq!(
            hyperlink_format("vscode").as_deref(),
            Some("vscode://file{path}:{line}:{column}")
        );
        assert_eq!(
            hyperlink_format("vscode-insiders").as_deref(),
            Some("vscode-insiders://file{path}:{line}:{column}")
        );
        assert_eq!(
            hyperlink_format("vscodium").as_deref(),
            Some("vscodium://file{path}:{line}:{column}")
        );
        assert_eq!(
            hyperlink_format("cursor").as_deref(),
            Some("cursor://file{path}:{line}:{column}")
        );
        assert_eq!(
            hyperlink_format("file").as_deref(),
            Some("file://{host}{path}")
        );
        assert_eq!(
            hyperlink_format("default").as_deref(),
            Some("file://{host}{path}")
        );
        assert_eq!(
            hyperlink_format("grep+").as_deref(),
            Some("grep+://{path}:{line}")
        );
        assert_eq!(
            hyperlink_format("kitty").as_deref(),
            Some("file://{host}{path}#{line}")
        );
        assert_eq!(
            hyperlink_format("macvim").as_deref(),
            Some("mvim://open?url=file://{path}&line={line}&column={column}")
        );
        assert_eq!(
            hyperlink_format("textmate").as_deref(),
            Some("txmt://open?url=file://{path}&line={line}&column={column}")
        );
        assert_eq!(
            hyperlink_format("custom://open/{path}:{line}").as_deref(),
            Some("custom://open/{path}:{line}")
        );
    }

    #[test]
    fn test_hyperlink_url_defaults_column_to_one() {
        assert_eq!(
            hyperlink_url("vscode://file{path}:{line}:{column}", "/tmp/a.txt", 7, 0),
            "vscode://file/tmp/a.txt:7:1"
        );
    }

    #[test]
    fn test_hyperlink_url_substitutes_real_column_when_provided() {
        assert_eq!(
            hyperlink_url("vscode://file{path}:{line}:{column}", "/tmp/a.txt", 42, 13),
            "vscode://file/tmp/a.txt:42:13"
        );
    }

    #[test]
    fn test_hyperlink_url_substitutes_column_for_per_line_links() {
        // Regression: per-line diff hyperlinks were leaving `{column}` literal
        // in the URL, which terminals then percent-encoded as `%7Bcolumn%7D`.
        let url = hyperlink_url("vscode://file{path}:{line}:{column}", "/tmp/cli.rs", 808, 0);
        assert_eq!(url, "vscode://file/tmp/cli.rs:808:1");
    }

    #[test]
    fn test_hyperlink_format_disables_for_empty_and_none() {
        assert_eq!(hyperlink_format(""), None);
        assert_eq!(hyperlink_format("   "), None);
        assert_eq!(hyperlink_format("none"), None);
        assert_eq!(hyperlink_format("NONE"), None);
    }

    #[test]
    fn test_hyperlink_url_defaults_zero_line_to_one() {
        assert_eq!(
            hyperlink_url("vscode://file{path}:{line}", "/tmp/a.txt", 0, 0),
            "vscode://file/tmp/a.txt:1"
        );
    }

    #[test]
    fn test_percent_encode_path_preserves_unreserved_and_path_separators() {
        assert_eq!(percent_encode_path("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(percent_encode_path("/a-b_c.d~e/f"), "/a-b_c.d~e/f");
        // ":" is preserved as a URI authority/segment separator.
        assert_eq!(percent_encode_path("/srv:8080/a"), "/srv:8080/a");
    }

    #[test]
    fn test_percent_encode_path_encodes_special_chars() {
        assert_eq!(
            percent_encode_path("/tmp/notes (draft).md"),
            "/tmp/notes%20%28draft%29.md"
        );
        assert_eq!(percent_encode_path("/tmp/file#1.txt"), "/tmp/file%231.txt");
        assert_eq!(percent_encode_path("/tmp/a?b"), "/tmp/a%3Fb");
        assert_eq!(percent_encode_path("/tmp/100%.txt"), "/tmp/100%25.txt");
    }

    #[test]
    fn test_percent_encode_path_passes_utf8_through() {
        assert_eq!(percent_encode_path("/tmp/café.txt"), "/tmp/café.txt");
    }

    #[test]
    fn test_hyperlink_url_substitutes_host() {
        // `{host}` is replaced by the system hostname (or empty if unresolvable).
        let url = hyperlink_url("file://{host}{path}", "/tmp/a.txt", 0, 0);
        assert!(url.starts_with("file://"));
        assert!(url.ends_with("/tmp/a.txt"));
    }

    #[test]
    fn test_hyperlink_url_passes_format_through_when_no_placeholders() {
        // Format without placeholders is returned verbatim - the gating in
        // `hyperlink_url` short-circuits all replace/encode/hostname calls.
        assert_eq!(
            hyperlink_url("https://example.com/static", "/tmp/a.txt", 42, 7),
            "https://example.com/static"
        );
    }

    #[test]
    fn test_hyperlink_url_skips_unused_placeholders() {
        // Format omitting `{line}`, `{column}`, and `{host}` produces output
        // that contains no expansion of those placeholders even when the
        // caller passes real values for them.
        let url = hyperlink_url("file://{path}", "/tmp/a.txt", 42, 7);
        assert_eq!(url, "file:///tmp/a.txt");
        assert!(!url.contains("42"));
        assert!(!url.contains(":7"));
    }

    #[test]
    fn test_hyperlink_url_path_only_skips_line_substitution() {
        // A format with `{path}` but no `{line}` should not append/inject
        // a line number anywhere - confirms the `{line}` gate is correct.
        assert_eq!(
            hyperlink_url("vscode://file{path}", "/tmp/a.txt", 99, 0),
            "vscode://file/tmp/a.txt"
        );
    }
}

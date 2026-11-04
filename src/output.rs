//! Rendering and ordered output delivery.

use crate::diff;
use crate::expressions::Replacement;
use crate::hyperlink::{HyperlinkTemplate, hyperlink_with_template, percent_encode_path};
use crate::ui::{Color, Styles};
use diffy::DiffOptions;
use std::io::{self, Seek as _, Write as _};

pub(crate) const BUFFER_CAPACITY: usize = 256 * 1024;

pub(crate) struct ReplacementResult {
    pub(crate) path: String,
    pub(crate) link_path: String,
    pub(crate) count: usize,
    pub(crate) diff: Option<(String, String)>,
    /// 1-indexed `(line, first-match column)` pairs for the original file,
    /// sorted by line, used to fill `{column}` in per-line hyperlinks. `None`
    /// when position tracking was disabled (no `{column}` placeholder in the
    /// hyperlink format).
    pub(crate) columns: Option<Vec<(usize, usize)>>,
    /// Same map keyed by the rewritten file's line numbers, for `{column}` on
    /// the new side of a line-shifting diff. `None` unless the replacement adds
    /// or removes newlines (the only case where new line numbers diverge).
    pub(crate) out_columns: Option<Vec<(usize, usize)>>,
    /// Per-replacement input/output spans, populated when span tracking is
    /// enabled and the run uses a single expression. Drives inline highlight.
    pub(crate) spans: Vec<Replacement>,
    /// True when colored diff can compare old/new lines by number instead of
    /// running a full LCS. Only enabled for replacements that cannot affect
    /// line boundaries.
    pub(crate) linewise_diff: bool,
    /// True when colored diff can render newline-changing replacements from
    /// replacement spans instead of falling back to a full LCS.
    pub(crate) multiline_span_diff: bool,
}

fn format_count<F>(n: usize, format: &F) -> String
where
    F: num_format::Format,
{
    use num_format::ToFormattedString as _;
    n.to_formatted_string(format)
}

#[cfg(not(windows))]
fn has_ambiguous_digit_group_separator(separator: &str) -> bool {
    separator.chars().all(char::is_whitespace)
}

#[cfg(not(windows))]
fn with_commas(n: usize) -> String {
    use std::sync::OnceLock;
    static SYSTEM_LOCALE: OnceLock<Option<num_format::SystemLocale>> = OnceLock::new();
    let cached = SYSTEM_LOCALE.get_or_init(|| {
        let loc = num_format::SystemLocale::default().ok()?;
        if has_ambiguous_digit_group_separator(loc.separator()) {
            return None;
        }
        Some(loc)
    });
    match cached {
        Some(loc) => format_count(n, loc),
        None => format_count(n, &num_format::Locale::en),
    }
}

// Windows has no `num-format` system-locale support (it needs the Windows SDK at
// build time, unavailable when cross-compiling), so use the static English
// locale.
#[cfg(windows)]
fn with_commas(n: usize) -> String {
    format_count(n, &num_format::Locale::en)
}

fn summary_message_with_formatter<F>(
    total_files: usize,
    total_matches: usize,
    delete: bool,
    dry: bool,
    format_count: F,
) -> String
where
    F: Fn(usize) -> String,
{
    let verb = if dry { "Would perform" } else { "Performed" };
    let noun = if delete { "deletion" } else { "replacement" };
    format!(
        "{} {} {noun}{} in {} file{}",
        verb,
        format_count(total_matches),
        if total_matches == 1 { "" } else { "s" },
        format_count(total_files),
        if total_files == 1 { "" } else { "s" },
    )
}

fn summary_message(total_files: usize, total_matches: usize, delete: bool, dry: bool) -> String {
    summary_message_with_formatter(total_files, total_matches, delete, dry, with_commas)
}

pub(crate) struct ResultPrinter<'a> {
    pub(crate) styles: Styles<'a>,
    pub(crate) quiet: bool,
    pub(crate) delete: bool,
    pub(crate) dry: bool,
    pub(crate) no_hints: bool,
    pub(crate) hyperlink_format: Option<&'a str>,
    pub(crate) hyperlink_limit: u64,
    pub(crate) context_lines: usize,
}

impl ResultPrinter<'_> {
    pub(crate) fn render(&self, result: &ReplacementResult, colored: bool) -> io::Result<Rendered> {
        if !colored {
            let mut plain = Vec::new();
            if !self.quiet {
                self.write_patch_results_to(std::slice::from_ref(result), &mut plain)?;
            }
            return Ok(Rendered {
                count: result.count,
                plain,
                linked: None,
            });
        }
        let template = self
            .hyperlink_format
            .filter(|_| !self.styles.is_plain())
            .map(HyperlinkTemplate::parse);
        if self.hyperlink_limit == 0 {
            return Ok(Rendered {
                count: result.count,
                plain: self.render_block(result, self.styles, template.as_ref()),
                linked: None,
            });
        }
        let plain = self.render_block(result, self.styles, None);
        let linked = template
            .as_ref()
            .map(|t| self.render_block(result, self.styles, Some(t)));
        Ok(Rendered {
            count: result.count,
            plain,
            linked,
        })
    }

    fn summary(&self, files: usize, matches: usize, out: &mut impl io::Write) -> io::Result<()> {
        if files == 0 {
            return Ok(());
        }
        let styles = self.styles;
        let color = if self.dry {
            Color::Yellow
        } else {
            Color::Green
        };
        let msg = summary_message(files, matches, self.delete, self.dry);
        let hint = if self.dry && !self.no_hints {
            let yellow = styles.fg(Color::Yellow);
            let green = styles.fg(Color::Green);
            let dim = styles.dim();
            let reset = styles.reset();
            format!(
                " {yellow}{dim}(pass {reset}{green}{dim}--write{reset}{yellow}{dim} to apply){reset}"
            )
        } else {
            String::new()
        };
        writeln!(
            out,
            "
{}{}{}{}{hint}",
            styles.bold(),
            styles.fg(color),
            msg,
            styles.reset()
        )
    }

    pub(crate) fn render_block(
        &self,
        result: &ReplacementResult,
        styles: Styles,
        template: Option<&HyperlinkTemplate<'_>>,
    ) -> Vec<u8> {
        use std::fmt::Write as _;

        let mut buf = String::new();
        let count = with_commas(result.count);
        let encoded_path = template
            .filter(|t| t.uses_path())
            .map_or(String::new(), |_| percent_encode_path(&result.link_path));
        let path = hyperlink_with_template(template, &encoded_path, 0, &result.path);
        let _ = writeln!(
            buf,
            "{}{} {}({count}){}",
            if self.quiet {
                ""
            } else {
                styles.fg(Color::Magenta)
            },
            path,
            styles.fg(Color::Grey),
            styles.reset()
        );

        if !self.quiet
            && let Some((old, new)) = &result.diff
        {
            diff::print_file_line_diff(
                old,
                new,
                diff::DiffHints {
                    spans: &result.spans,
                    linewise: result.linewise_diff,
                    multiline_spans: result.multiline_span_diff,
                },
                styles,
                template,
                &encoded_path,
                result.columns.as_deref(),
                result.out_columns.as_deref(),
                // In dry-run the file on disk is the original (old side); after
                // a write it is the rewritten version (new side). Per-line links
                // on line-shifting diffs target whichever side is on disk.
                if self.dry { Color::Red } else { Color::Green },
                &mut buf,
            );
        }
        buf.into_bytes()
    }

    pub(crate) fn write_patch_results_to<W: std::io::Write>(
        &self,
        results: &[ReplacementResult],
        out: &mut W,
    ) -> std::io::Result<()> {
        for result in results {
            let Some((old, new)) = &result.diff else {
                continue;
            };
            let mut options = DiffOptions::new();
            options
                .set_context_len(self.context_lines)
                .set_original_filename(format!("a/{}", result.path))
                .set_modified_filename(format!("b/{}", result.path));
            let patch = options.create_patch(old, new);
            write!(out, "{patch}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    struct FailingWriter {
        fail_on_flush: bool,
    }
    impl io::Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_on_flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::other("write failed"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    fn printer() -> ResultPrinter<'static> {
        ResultPrinter {
            styles: Styles::PLAIN,
            quiet: false,
            delete: false,
            dry: true,
            no_hints: true,
            hyperlink_format: None,
            hyperlink_limit: 0,
            context_lines: 3,
        }
    }

    #[test]
    fn output_write_failure_is_returned_by_finish() {
        let printer = printer();
        let mut sink = Sink::new(
            &printer,
            FailingWriter {
                fail_on_flush: false,
            },
            false,
        );
        assert!(!sink.push(&Rendered {
            count: 1,
            plain: b"patch".to_vec(),
            linked: None
        }));
        assert_eq!(sink.finish().unwrap_err().to_string(), "write failed");
    }

    #[test]
    fn final_flush_failure_is_returned() {
        let printer = printer();
        let mut sink = Sink::new(
            &printer,
            FailingWriter {
                fail_on_flush: true,
            },
            false,
        );
        assert!(sink.push(&Rendered {
            count: 1,
            plain: b"patch".to_vec(),
            linked: None
        }));
        assert_eq!(sink.finish().unwrap_err().to_string(), "flush failed");
    }
}

pub(crate) struct Rendered {
    pub(crate) count: usize,
    plain: Vec<u8>,
    linked: Option<Vec<u8>>,
}

/// Only the global hyperlink decision requires delayed output. Spools hold
/// rendered bytes, never source buffers or replacement spans.
struct Spools {
    plain: std::fs::File,
    linked: Option<std::fs::File>,
}

pub(crate) struct Sink<'a, W> {
    printer: &'a ResultPrinter<'a>,
    out: W,
    colored: bool,
    files: usize,
    matches: usize,
    spools: Option<Spools>,
    error: Option<io::Error>,
}

impl<'a, W: io::Write> Sink<'a, W> {
    pub(crate) fn new(printer: &'a ResultPrinter<'a>, out: W, colored: bool) -> Self {
        let spools = if colored
            && !printer.styles.is_plain()
            && printer.hyperlink_format.is_some()
            && printer.hyperlink_limit > 0
        {
            Some((|| -> io::Result<Spools> {
                Ok(Spools {
                    plain: tempfile::tempfile()?,
                    linked: Some(tempfile::tempfile()?),
                })
            })())
        } else {
            None
        };
        let (spools, error) = match spools.transpose() {
            Ok(spools) => (spools, None),
            Err(error) => (None, Some(error)),
        };
        Self {
            printer,
            out,
            colored,
            files: 0,
            matches: 0,
            spools,
            error,
        }
    }

    pub(crate) fn push(&mut self, result: &Rendered) -> bool {
        self.matches = self.matches.saturating_add(result.count);
        let separator = self.colored && !self.printer.quiet && self.files > 0;
        self.files += 1;
        if self.error.is_some() {
            return false;
        }
        let write_block = |out: &mut dyn io::Write, bytes: &[u8]| -> io::Result<()> {
            if separator {
                out.write_all(
                    b"
",
                )?;
            }
            out.write_all(bytes)
        };
        let written = if let Some(spools) = &mut self.spools {
            if u64::try_from(self.matches).unwrap_or(u64::MAX) > self.printer.hyperlink_limit {
                spools.linked = None;
            }
            write_block(&mut spools.plain, &result.plain).and_then(|()| {
                if let Some(linked) = &mut spools.linked {
                    write_block(linked, result.linked.as_deref().unwrap_or(&result.plain))?;
                }
                Ok(())
            })
        } else {
            write_block(
                &mut self.out,
                result.linked.as_deref().unwrap_or(&result.plain),
            )
        };
        if let Err(error) = written {
            self.error = Some(error);
            false
        } else {
            true
        }
    }

    pub(crate) fn finish(mut self) -> io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if let Some(spools) = self.spools.as_mut() {
            let selected = spools.linked.as_mut().unwrap_or(&mut spools.plain);
            selected.flush()?;
            selected.rewind()?;
            io::copy(selected, &mut self.out)?;
        }
        if self.colored {
            self.printer
                .summary(self.files, self.matches, &mut self.out)?;
        }
        self.out.flush()
    }
}

#[cfg(test)]
impl ResultPrinter<'_> {
    fn write_colored_results_to<W: io::Write>(
        &self,
        results: &[ReplacementResult],
        out: &mut W,
    ) -> io::Result<()> {
        let mut sink = Sink::new(self, out, true);
        for result in results {
            if !sink.push(&self.render(result, true)?) {
                break;
            }
        }
        sink.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_summary_message_singular() {
        assert_eq!(
            summary_message(1, 1, false, false),
            "Performed 1 replacement in 1 file"
        );
    }

    #[test]
    fn test_summary_message_plural() {
        assert_eq!(
            summary_message(2, 5, false, false),
            "Performed 5 replacements in 2 files"
        );
    }

    #[test]
    fn test_summary_message_dry_run_uses_would_perform() {
        assert_eq!(
            summary_message(1, 1, false, true),
            "Would perform 1 replacement in 1 file"
        );
    }

    #[test]
    fn test_summary_message_delete_uses_deletion() {
        assert_eq!(
            summary_message(1, 1, true, false),
            "Performed 1 deletion in 1 file"
        );
        assert_eq!(
            summary_message(2, 5, true, false),
            "Performed 5 deletions in 2 files"
        );
        assert_eq!(
            summary_message(1, 3, true, true),
            "Would perform 3 deletions in 1 file"
        );
    }

    #[test]
    fn test_patch_results_write_to_single_writer() {
        let printer = ResultPrinter {
            styles: Styles::ansi(),
            quiet: false,
            delete: false,
            dry: true,
            no_hints: false,
            hyperlink_format: None,
            hyperlink_limit: 0,
            context_lines: 3,
        };
        let results = [ReplacementResult {
            path: "a.txt".to_string(),
            link_path: "a.txt".to_string(),
            count: 1,
            diff: Some(("foo\n".to_string(), "bar\n".to_string())),
            columns: None,
            out_columns: None,
            spans: Vec::new(),
            linewise_diff: false,
            multiline_span_diff: false,
        }];
        let mut out = Vec::new();

        printer.write_patch_results_to(&results, &mut out).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-foo
+bar
"
        );
    }

    #[test]
    fn test_patch_results_write_through_buffered_writer() {
        let printer = ResultPrinter {
            styles: Styles::ansi(),
            quiet: false,
            delete: false,
            dry: true,
            no_hints: false,
            hyperlink_format: None,
            hyperlink_limit: 0,
            context_lines: 3,
        };
        let results = [ReplacementResult {
            path: "a.txt".to_string(),
            link_path: "a.txt".to_string(),
            count: 1,
            diff: Some(("foo\n".to_string(), "bar\n".to_string())),
            columns: None,
            out_columns: None,
            spans: Vec::new(),
            linewise_diff: false,
            multiline_span_diff: false,
        }];
        let mut out = Vec::new();
        {
            let mut buffered = std::io::BufWriter::new(&mut out);
            printer
                .write_patch_results_to(&results, &mut buffered)
                .unwrap();
            std::io::Write::flush(&mut buffered).unwrap();
        }

        assert!(String::from_utf8(out).unwrap().contains("-foo\n+bar\n"));
    }

    #[test]
    fn test_colored_results_write_through_buffered_writer() {
        let printer = ResultPrinter {
            styles: Styles::ansi(),
            quiet: false,
            delete: false,
            dry: true,
            no_hints: false,
            hyperlink_format: None,
            hyperlink_limit: 0,
            context_lines: 3,
        };
        let results = [ReplacementResult {
            path: "a.txt".to_string(),
            link_path: "a.txt".to_string(),
            count: 1,
            diff: Some(("foo\n".to_string(), "bar\n".to_string())),
            columns: None,
            out_columns: None,
            spans: Vec::new(),
            linewise_diff: false,
            multiline_span_diff: false,
        }];
        let mut out = Vec::new();
        {
            let mut buffered = std::io::BufWriter::new(&mut out);
            printer
                .write_colored_results_to(&results, &mut buffered)
                .unwrap();
            std::io::Write::flush(&mut buffered).unwrap();
        }

        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("a.txt"), "expected path in output: {s:?}");
        assert!(s.contains("(1)"), "expected match count in output: {s:?}");
    }

    #[test]
    fn test_parallel_render_stream_preserves_result_order() {
        let printer = ResultPrinter {
            styles: Styles::ansi(),
            quiet: false,
            delete: false,
            dry: true,
            no_hints: true,
            hyperlink_format: None,
            hyperlink_limit: 0,
            context_lines: 3,
        };
        let results: Vec<_> = (0..32)
            .map(|idx| ReplacementResult {
                path: format!("file-{idx:02}.txt"),
                link_path: String::new(),
                count: 1,
                diff: None,
                columns: None,
                out_columns: None,
                spans: Vec::new(),
                linewise_diff: false,
                multiline_span_diff: false,
            })
            .collect();
        let mut out = Vec::new();

        printer
            .write_colored_results_to(&results, &mut out)
            .unwrap();

        let out = String::from_utf8(out).unwrap();
        let mut cursor = 0;
        for idx in 0..32 {
            let path = format!("file-{idx:02}.txt");
            let offset = out[cursor..]
                .find(&path)
                .unwrap_or_else(|| panic!("missing ordered path {path:?} in {out:?}"));
            cursor += offset + path.len();
        }
    }

    #[test]
    fn test_format_count_uses_requested_locale() {
        assert_eq!(format_count(0, &num_format::Locale::en), "0");
        assert_eq!(format_count(7, &num_format::Locale::en), "7");
        assert_eq!(format_count(999, &num_format::Locale::en), "999");
        assert_eq!(format_count(1_000, &num_format::Locale::en), "1,000");
        assert_eq!(format_count(12_345, &num_format::Locale::en), "12,345");
        assert_eq!(format_count(648_098, &num_format::Locale::en), "648,098");
        assert_eq!(
            format_count(1_000_000, &num_format::Locale::en),
            "1,000,000"
        );
    }

    #[test]
    fn test_has_ambiguous_digit_group_separator() {
        assert!(!has_ambiguous_digit_group_separator(","));
        assert!(has_ambiguous_digit_group_separator(" "));
        assert!(has_ambiguous_digit_group_separator("\u{00a0}"));
    }

    #[test]
    fn test_with_commas_preserves_small_values_without_grouping() {
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(7), "7");
        assert_eq!(with_commas(999), "999");
    }

    #[test]
    fn test_summary_message_large_counts_use_thousands_separators() {
        assert_eq!(
            summary_message_with_formatter(718, 648_098, false, false, |n| {
                format_count(n, &num_format::Locale::en)
            }),
            "Performed 648,098 replacements in 718 files"
        );
        assert_eq!(
            summary_message_with_formatter(1_000, 2_500_000, false, true, |n| {
                format_count(n, &num_format::Locale::en)
            }),
            "Would perform 2,500,000 replacements in 1,000 files"
        );
    }
}

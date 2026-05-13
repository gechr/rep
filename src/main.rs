mod config;
mod diff;
mod expressions;
mod interactive;
mod scan;
mod theme;
mod ui;

use std::io::IsTerminal as _;
use std::path::PathBuf;

/// True when stdin is a pipe or redirected regular file. TTY, `/dev/null`,
/// and sockets all return false - `is_terminal()` alone can't distinguish a
/// real pipe from `/dev/null`. Sockets are excluded so IPC test harnesses
/// don't trigger stdin mode.
#[cfg(unix)]
fn stdin_has_input() -> bool {
    use std::fs::File;
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::FileTypeExt as _;

    let fd = std::io::stdin().as_raw_fd();
    // SAFETY: ManuallyDrop keeps fd 0 open after the borrow.
    let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let Ok(meta) = file.metadata() else {
        return false;
    };
    meta.file_type().is_fifo() || meta.is_file()
}

#[cfg(not(unix))]
fn stdin_has_input() -> bool {
    !std::io::stdin().is_terminal()
}

use anyhow::{Result, bail};
use clap::{CommandFactory as _, Parser};
use clap_complete::Shell;
use diffy::DiffOptions;

use crate::expressions::{
    CompiledExpression, EXPR_SEP, Replacement, apply_compiled_expressions,
    build_pre_filter_matcher, compile_expressions, first_column_map_if_needed,
};
use crate::ui::Color;
use crate::ui::ColorChoice;
use crate::ui::Styles;

struct ReplacementResult {
    path: String,
    link_path: String,
    count: usize,
    diff: Option<(String, String)>,
    /// 1-indexed `(line -> first-match column)` for the original file, used to
    /// fill `{column}` in per-line hyperlinks. Empty when position tracking
    /// was disabled (non-interactive output).
    columns: std::collections::HashMap<usize, usize>,
    /// Per-replacement input/output spans, populated when span tracking is
    /// enabled and the run uses a single expression. Drives inline highlight.
    spans: Vec<Replacement>,
    /// True when colored diff can compare old/new lines by number instead of
    /// running a full LCS. Only enabled for replacements that cannot affect
    /// line boundaries.
    linewise_diff: bool,
    /// True when colored diff can render newline-changing replacements from
    /// replacement spans instead of falling back to a full LCS.
    multiline_span_diff: bool,
}

#[derive(Parser)]
#[command(name = "rep", version, disable_help_flag = true)]
struct Cli {
    #[arg(value_name = "arg")]
    args: Vec<String>,

    #[arg(
        short = 'f',
        long = "files",
        value_name = "glob",
        help = "Smart glob patterns to match files against",
        help_heading = "Filter",
        overrides_with = "files"
    )]
    files: Option<String>,

    #[arg(
        short = 'H',
        long = "hidden",
        help = "Search hidden files and directories",
        help_heading = "Filter"
    )]
    hidden: bool,

    #[arg(
        long = "no-ignore",
        help = "Do not respect ignore files",
        help_heading = "Filter"
    )]
    no_ignore: bool,

    #[arg(
        short = 'e',
        long = "expression",
        value_name = "f> <r",
        help = "Repeatable <find> <replace> expression",
        help_heading = "Replace"
    )]
    expressions: Vec<String>,

    #[arg(
        short = 'S',
        long = "smart",
        overrides_with = "preserve",
        help = "Replace all case variants of the pattern",
        help_heading = "Replace"
    )]
    smart: bool,

    #[arg(
        short = 'P',
        long = "preserve",
        overrides_with = "smart",
        help = "Mirror the <find> case onto the <replace>",
        help_heading = "Replace"
    )]
    preserve: bool,

    #[arg(
        short = 'G',
        long = "greedy",
        help = "Use greedy matching for regular expressions",
        help_heading = "Match"
    )]
    greedy: bool,

    #[arg(
        short = 'i',
        long = "ignore-case",
        help = "Case-insensitive matching",
        help_heading = "Match"
    )]
    ignore_case: bool,

    #[arg(
        short = 'm',
        long = "multiline",
        help = "Search across multiple lines",
        help_heading = "Match"
    )]
    multiline: bool,

    #[arg(
        long = "dotall",
        help = "Allow dot to match newlines",
        help_heading = "Match"
    )]
    dotall: bool,

    #[arg(
        short = 'r',
        long = "regex",
        alias = "regexp",
        help = "Treat patterns as regular expressions",
        help_heading = "Match"
    )]
    regexp: bool,

    #[arg(
        short = 'w',
        long = "word-regexp",
        help = "Match only whole words",
        help_heading = "Match"
    )]
    word_regexp: bool,

    #[arg(
        short = 'x',
        long = "line-regexp",
        help = "Match only whole lines",
        help_heading = "Match"
    )]
    line_regexp: bool,

    #[arg(
        short = 'd',
        long = "delete",
        help = "Delete lines matching <find>",
        help_heading = "Replace"
    )]
    delete: bool,

    #[arg(
        short = 'n',
        long = "dry-run",
        alias = "dry",
        overrides_with_all = ["write", "preview"],
        help = "Show what would be changed without writing",
        help_heading = "Mode"
    )]
    dry_run: bool,

    #[arg(
        short = 'W',
        long = "write",
        overrides_with_all = ["dry_run", "preview"],
        help = "Apply changes to disk",
        help_heading = "Mode"
    )]
    write: bool,

    #[arg(
        short = 'p',
        long = "preview",
        overrides_with_all = ["dry_run", "write"],
        help = "Preview the changes before applying them",
        help_heading = "Mode"
    )]
    preview: bool,

    #[arg(
        long = "preview-tool",
        value_name = "cmd",
        requires = "preview",
        overrides_with = "preview_tool",
        help = "External diff tool for preview mode",
        help_heading = "Mode"
    )]
    preview_tool: Option<String>,

    #[arg(
        short = 'l',
        long = "list-files",
        help = "Print file paths that would be changed",
        help_heading = "Mode"
    )]
    list_files: bool,

    #[arg(
        short = 'C',
        long = "context",
        value_name = "n",
        default_value_t = DEFAULT_CONTEXT_LINES,
        overrides_with = "context",
        hide_short_help = true,
        help = "Lines of context in patch output and preview",
        help_heading = "Miscellaneous",
        display_order = 90
    )]
    context: usize,

    #[arg(
        long = "hyperlink-format",
        value_name = "fmt",
        overrides_with = "hyperlink_format",
        help = "Terminal hyperlink format",
        help_heading = "Miscellaneous",
        display_order = 100
    )]
    hyperlink_format: Option<String>,

    #[arg(
        long = "hyperlink-limit",
        value_name = "n",
        default_value_t = DEFAULT_HYPERLINK_LIMIT,
        overrides_with = "hyperlink_limit",
        allow_negative_numbers = true,
        hide_short_help = true,
        help = "Disable hyperlinks above this many matches (0 = unlimited)",
        help_heading = "Miscellaneous",
        display_order = 101
    )]
    hyperlink_limit: u64,

    #[arg(
        long = "color",
        alias = "colour",
        value_name = "when",
        value_enum,
        default_value_t = ColorChoice::Auto,
        overrides_with = "color",
        help = "When to use color",
        help_heading = "Miscellaneous",
        display_order = 95
    )]
    color: ColorChoice,

    #[arg(
        short = 'q',
        long = "quiet",
        help = "Suppress summary output",
        help_heading = "Miscellaneous",
        display_order = 110
    )]
    quiet: bool,

    #[arg(
        long = "style-added",
        value_name = "style",
        help = "Style for added lines",
        help_heading = "Style",
        display_order = 10
    )]
    style_added: Option<String>,

    #[arg(
        long = "style-removed",
        value_name = "style",
        help = "Style for removed lines",
        help_heading = "Style",
        display_order = 20
    )]
    style_removed: Option<String>,

    #[arg(
        long = "style-line-added",
        value_name = "style",
        help = "Style for added line numbers",
        help_heading = "Style",
        display_order = 30
    )]
    style_line_added: Option<String>,

    #[arg(
        long = "style-line-removed",
        value_name = "style",
        help = "Style for removed line numbers",
        help_heading = "Style",
        display_order = 40
    )]
    style_line_removed: Option<String>,

    #[arg(
        long = "marker-added",
        value_name = "str",
        help = "Marker before added lines",
        help_heading = "Style",
        display_order = 60
    )]
    marker_added: Option<String>,

    #[arg(
        long = "marker-removed",
        value_name = "str",
        help = "Marker before removed lines",
        help_heading = "Style",
        display_order = 70
    )]
    marker_removed: Option<String>,

    #[arg(
        short = 'h',
        help = "Print short help",
        help_heading = "Miscellaneous",
        display_order = 130
    )]
    help: bool,

    #[arg(
        long = "help",
        help = "Print long help with examples",
        help_heading = "Miscellaneous",
        display_order = 140
    )]
    help_long: bool,

    #[arg(long = "completions", value_name = "shell", hide = true)]
    completions: Option<Shell>,

    #[arg(long = "no-hints", overrides_with_all = ["hints", "no_hints"], hide = true)]
    no_hints: bool,

    #[arg(long = "hints", overrides_with_all = ["no_hints", "hints"], hide = true)]
    hints: bool,
}

const DEFAULT_CONTEXT_LINES: usize = 3;
const DEFAULT_HYPERLINK_LIMIT: u64 = 50_000;

const HELP_SECTIONS: &[&str] = &["Filter", "Match", "Replace", "Mode", "Miscellaneous"];
const LONG_HELP_SECTIONS: &[&str] = &[
    "Filter",
    "Match",
    "Replace",
    "Mode",
    "Style",
    "Miscellaneous",
];
const SECTION_SPACERS: &[&str] = &[
    "preview_tool",
    "hyperlink_format",
    "hyperlink_limit",
    "version",
];

/// Clap auto-assigns a `value_name` to every arg, including bool flags. Gate on
/// the action so `--quiet` doesn't render as `--quiet <QUIET>`.
fn arg_value_name(arg: &clap::Arg) -> Option<&str> {
    matches!(
        arg.get_action(),
        clap::ArgAction::Set | clap::ArgAction::Append
    )
    .then(|| {
        arg.get_value_names()
            .and_then(|v| v.first())
            .map(clap::builder::Str::as_str)
    })
    .flatten()
}

fn arg_body_width(arg: &clap::Arg) -> usize {
    let long_part = arg.get_long().map_or(0, |l| 4 + 2 + l.len());
    let short_only = arg.get_long().is_none() && arg.get_short().is_some();
    let flags = if short_only { 2 } else { long_part };
    let val = arg_value_name(arg).map_or(0, |v| 3 + v.len());
    flags + val
}

fn render_arg_body(arg: &clap::Arg, styles: Styles) -> String {
    use std::fmt::Write as _;

    let red = styles.fg(Color::Red);
    let dim_attr = styles.dim();
    let reset = styles.reset();

    let mut body = match (arg.get_short(), arg.get_long()) {
        (Some(c), Some(l)) => format!("{red}-{c}{reset}, {red}--{l}{reset}"),
        (None, Some(l)) => format!("    {red}--{l}{reset}"),
        (Some(c), None) => format!("{red}-{c}{reset}"),
        (None, None) => String::new(),
    };
    if let Some(v) = arg_value_name(arg) {
        let _ = write!(body, " {red}{dim_attr}<{v}>{reset}");
    }
    body
}

fn colorize_help_metavars(help: &str, styles: Styles) -> String {
    let blue = styles.fg(Color::Blue);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();
    let mut out = help
        .replace("<find>", &format!("{blue}<find>{reset}"))
        .replace("<replace>", &format!("{blue}<replace>{reset}"));
    if let Some(open) = out.rfind(" (")
        && out.ends_with(')')
    {
        let tail = &out[open + 1..];
        let styled = format!(" {grey}{tail}{reset}");
        out.truncate(open);
        out.push_str(&styled);
    }
    out
}

/// The id of the mode flag that "wins" given rc state - i.e. the flag that
/// would be active if no mode flag is passed on the CLI. Mirrors the dispatch
/// in `run`: `cli.write` first, then `cli.preview`, fallthrough to dry-run.
fn current_default_mode_id() -> &'static str {
    let mut id = "dry_run";
    for arg in config::rc_args() {
        match arg.to_str() {
            Some("-n" | "--dry-run" | "--dry") => id = "dry_run",
            Some("-W" | "--write") => id = "write",
            Some("-p" | "--preview") => id = "preview",
            _ => {}
        }
    }
    id
}

fn print_help() {
    print_help_with(HELP_SECTIONS, false);
}

fn print_help_with(sections: &[&str], long: bool) {
    let styles = ui::Styles::when(std::io::stdout().is_terminal());
    let bold = styles.bold();
    let red = styles.fg(Color::Red);
    let green = styles.fg(Color::Green);
    let yellow = styles.fg(Color::Yellow);
    let blue = styles.fg(Color::Blue);
    let magenta = styles.fg(Color::Magenta);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();

    print!(
        "\
{yellow}{bold}Usage{reset}

  {green}{bold}rep{reset} {red}[options]{reset} {blue}<find> <replace>{reset} {magenta}[<path>…]{reset}

    {blue}<find>{reset}     String to find
    {blue}<replace>{reset}  String to replace with
    {magenta}<path>…{reset}    Paths to search in {grey}(optional){reset}
"
    );

    let cmd = Cli::command();
    let default_mode = current_default_mode_id();

    // Synthesized for the renderer: the real `--version` is added by clap's
    // build pass, which `Cli::command()` doesn't trigger.
    let version_arg = clap::Arg::new("version")
        .short('V')
        .long("version")
        .help("Print version")
        .help_heading("Miscellaneous")
        .display_order(120);

    let mut visible: Vec<(usize, &clap::Arg)> = cmd
        .get_arguments()
        .enumerate()
        .filter(|(_, a)| !a.is_hide_set())
        .filter(|(_, a)| long || !a.is_hide_short_help_set())
        .filter(|(_, a)| a.get_help_heading().is_none_or(|h| sections.contains(&h)))
        .collect();
    visible.push((visible.len(), &version_arg));

    let cell = visible
        .iter()
        .map(|(_, a)| arg_body_width(a))
        .max()
        .unwrap_or(0);

    for section in sections {
        let mut rows: Vec<(usize, &clap::Arg)> = visible
            .iter()
            .filter(|(_, a)| a.get_help_heading() == Some(*section))
            .copied()
            .collect();
        // Promote the active default mode flag to the top of its section.
        rows.sort_by_key(|(idx, a)| {
            let is_default = a.get_id().as_str() == default_mode;
            (!is_default, a.get_display_order(), *idx)
        });
        if rows.is_empty() {
            continue;
        }

        println!();
        println!("{yellow}{bold}{section}{reset}");
        println!();

        let mut iter = rows.iter().peekable();
        while let Some((_, arg)) = iter.next() {
            let body = render_arg_body(arg, styles);
            let pad = (cell + 2).saturating_sub(arg_body_width(arg)).max(2);
            let help_text = arg.get_help().map(ToString::to_string).unwrap_or_default();
            let help = colorize_help_metavars(&help_text, styles);
            let suffix = if arg.get_id().as_str() == default_mode {
                format!(" {grey}(default){reset}")
            } else {
                String::new()
            };
            println!("  {body}{}{help}{suffix}", " ".repeat(pad));

            // Suppress the blank when the next visible row is itself a
            // spacer entry: this keeps tight groups (e.g. --hyperlink-format
            // and --hyperlink-limit) visually together while still inserting
            // a single blank after the group ends.
            if SECTION_SPACERS.contains(&arg.get_id().as_str())
                && iter
                    .peek()
                    .is_none_or(|(_, next)| !SECTION_SPACERS.contains(&next.get_id().as_str()))
            {
                println!();
            }
        }
    }
}

fn print_help_long() {
    let styles = ui::Styles::when(std::io::stdout().is_terminal());
    let bold = styles.bold();
    let green = styles.fg(Color::Green);
    let yellow = styles.fg(Color::Yellow);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();

    print_help_with(LONG_HELP_SECTIONS, true);

    let text = format!(
        "
{yellow}{bold}Examples{reset}

  {grey}# Replace \"1.2.3\" with \"4.5.6\" in all files{reset}
  {green}${reset} rep 1.2.3 4.5.6

  {grey}# Replace \"foo\" with \"bar\" in \"*.txt\" files{reset}
  {green}${reset} rep -f txt foo bar

  {grey}# Replace \"foo\" with \"bar\" in all (hidden) files{reset}
  {green}${reset} rep --hidden foo bar

  {grey}# Replace \"foo\" with \"bar\" in all (hidden) Dockerfiles{reset}
  {green}${reset} rep -f '=Dockerfile' --hidden foo bar

  {grey}# Replace \"foo\" with \"bar\" in all files and preview changes{reset}
  {green}${reset} rep --preview foo bar

  {grey}# Replace \"1.2.3\" and \"3.2.1\" with \"4.5.6\" in all files{reset}
  {green}${reset} rep --regex '[13]\\.2\\.[13]' 4.5.6

  {grey}# Swap \"foo.bar\" with \"bar.foo\" in all files{reset}
  {green}${reset} rep --regex '(foo)\\.(bar)' '$2.$1'

  {grey}# Replace \"f.oo\" and \"F.OO\" with \"bar\"{reset}
  {green}${reset} rep --ignore-case 'f.oo' bar

  {grey}# Smart-replace in all files:
  {grey}#  \"foo_bar\" with \"hello_world\"{reset}
  {grey}#  \"FooBar\"  with \"HelloWorld\"{reset}
  {grey}#  \"FOO_BAR\" with \"HELLO_WORLD\"{reset}
  {green}${reset} rep --smart foo_bar hello_world

  {grey}# Read from stdin and replace \"foo\" with \"bar\"{reset}
  {green}${reset} echo foo bar | rep foo bar
  {green}${reset} rep foo bar < foobar.txt

  {grey}# Apply multiple replacements in one pass{reset}
  {green}${reset} rep -e foo bar -e baz qux src

  {grey}# Delete every line containing \"TODO\"{reset}
  {green}${reset} rep -d TODO
"
    );
    print!("{text}");
}

impl Cli {
    const fn uses_expressions(&self) -> bool {
        !self.expressions.is_empty()
    }

    /// True when the CLI cannot take a `<replace>` positional.
    ///
    /// `-d`/`--delete` forbids `<replace>`; trailing positionals are paths.
    /// `-l` alone accepts an optional `<replace>`, so it is not find-only;
    /// see [`Self::positional_skip`].
    const fn is_find_only(&self) -> bool {
        !self.uses_expressions() && self.delete
    }

    fn preview_tool(&self) -> Option<String> {
        if let Some(ref tool) = self.preview_tool {
            return Some(tool.clone());
        }
        // Default to delta if available on PATH
        if std::process::Command::new("delta")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Some("delta".to_string());
        }
        None
    }

    const fn is_regex(&self) -> bool {
        self.regexp
            || self.dotall
            || self.multiline
            || self.ignore_case
            || self.greedy
            || self.word_regexp
            || self.line_regexp
    }

    fn positional_skip(&self) -> usize {
        if self.uses_expressions() {
            0
        } else if self.is_find_only() {
            1
        } else if self.list_files {
            self.args.len().min(2)
        } else {
            2
        }
    }

    fn dirs(&self) -> Vec<&str> {
        let args = &self.args[self.positional_skip()..];
        if args.is_empty() {
            vec!["."]
        } else {
            args.iter().map(std::string::String::as_str).collect()
        }
    }

    fn file_set(&self) -> Option<scan::FileSet> {
        let globs = parse_file_globs(self.files.as_deref()?);
        if globs.is_empty() {
            return None;
        }
        Some(scan::FileSet {
            matches: globs,
            case_insensitive: true,
        })
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.args
            .iter()
            .skip(self.positional_skip())
            .map(PathBuf::from)
            .collect()
    }

    fn pattern(&self) -> &str {
        &self.args[0]
    }

    fn replacement(&self) -> &str {
        &self.args[1]
    }

    /// Returns the positional `<replace>` if one was supplied. Distinguishes
    /// `rep -l foo` (no replace, find-only listing) from `rep -l foo bar`
    /// (replace present, list only files where the replacement would change
    /// bytes).
    fn positional_replace(&self) -> Option<&str> {
        (!self.is_find_only() && self.args.len() >= 2).then(|| self.replacement())
    }
}

/// Preprocess argv so that `-e <find> <replace>` is compacted into a single
/// clap value joined by `EXPR_SEP` before clap parses the argument list.
/// This lets the second arg start with `-` without being treated as a flag.
///
/// Under `-d`/`--delete` there is no replace half, so `-e` consumes only a
/// single `<find>` token and any trailing positional is left as a path.
pub(crate) fn preprocess_expression_args(args: Vec<String>) -> Vec<String> {
    let delete_mode = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "-d" || a == "--delete");
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "-e" || arg == "--expression" {
            out.push(arg);
            let Some(find) = iter.next() else { continue };
            if delete_mode {
                out.push(find);
                continue;
            }
            let Some(replace) = iter.next() else {
                out.push(find);
                continue;
            };
            out.push(format!("{find}{EXPR_SEP}{replace}"));
        } else if let Some(find) = arg.strip_prefix("-e").filter(|s| !s.is_empty()) {
            // Compact form: -efoo -> find="foo", next arg is replace
            out.push("-e".to_string());
            if delete_mode {
                out.push(find.to_string());
                continue;
            }
            let Some(replace) = iter.next() else {
                out.push(find.to_string());
                continue;
            };
            out.push(format!("{find}{EXPR_SEP}{replace}"));
        } else {
            out.push(arg);
        }
    }
    out
}

fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

fn hyperlink_path(path: &std::path::Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    abs.to_string_lossy().to_string()
}

pub(crate) fn osc8(url: &str, text: &str) -> String {
    if ui::Styles::when(true).is_plain() {
        return text.to_string();
    }
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Resolves an alias or a literal format string to the format that
/// `hyperlink_url` consumes. `None` means "hyperlinks disabled" (the user
/// passed an empty string or the `none` alias).
fn hyperlink_format(value: &str) -> Option<String> {
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

fn hyperlink_format_uses_column(format: &str) -> bool {
    format.contains("{column}")
}

/// System hostname, cached. `None` if it can't be resolved or isn't UTF-8.
fn hostname() -> Option<&'static str> {
    static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOST.get_or_init(|| gethostname::gethostname().into_string().ok())
        .as_deref()
}

/// Percent-encodes a path per RFC 3986 §2.3 unreserved set, plus `/` and `:`
/// (preserved as path separators). Bytes >= 128 (UTF-8 continuations) pass
/// through unencoded.
fn percent_encode_path(s: &str) -> String {
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
}

impl<'a> HyperlinkTemplate<'a> {
    pub(crate) fn parse(format: &'a str) -> Self {
        let mut segs: Vec<HyperlinkSeg<'a>> = Vec::new();
        let mut has_path = false;
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
                "line" => HyperlinkSeg::Line,
                "column" => HyperlinkSeg::Column,
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
        Self { segs, has_path }
    }

    pub(crate) const fn uses_path(&self) -> bool {
        self.has_path
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

fn hyperlink_with_template(
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

/// Parse the `-f` smart glob mini-DSL into the iglob patterns consumed
/// by `scan::walk_builder_with_file_set`.
///
/// Supports comma-separated patterns:
///   `txt`         -> `*.txt`        (extension)
///   `=Dockerfile` -> `Dockerfile`   (exact filename)
///   `!=Makefile`  -> `!Makefile`    (exclude exact filename)
///   `*.json`      -> `*.json`       (glob as-is)
///   `!rs`         -> `!*.rs`        (exclude extension)
fn parse_file_globs(input: &str) -> Vec<String> {
    let mut globs = Vec::new();
    for part in input.split(',') {
        let pattern = part.trim();
        if pattern.is_empty() || pattern == "." {
            continue;
        }
        let glob = if let Some(rest) = pattern.strip_prefix("!=") {
            format!("!{rest}")
        } else if let Some(rest) = pattern.strip_prefix('=') {
            rest.to_string()
        } else if pattern.contains('*') {
            pattern.to_string()
        } else if let Some(rest) = pattern.strip_prefix('!') {
            format!("!*.{rest}")
        } else {
            format!("*.{pattern}")
        };
        if !glob.is_empty() {
            globs.push(glob);
        }
    }
    globs
}

fn run_list_files(cli: &Cli) -> Result<()> {
    use std::sync::mpsc::channel;
    use std::thread;

    use ignore::WalkState;

    use std::sync::Arc;

    let expressions = Arc::new(compile_expressions(cli)?);
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;
    let filter_by_change = cli.positional_replace().is_some();

    let dirs = cli.dirs();
    let mut builder = scan::walk_builder_with_file_set(&dirs, cli.file_set())?;
    scan::apply_walk_flags(&mut builder, cli.hidden, cli.no_ignore);
    let walk = builder
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        ))
        .build_parallel();

    let (tx, rx) = channel::<String>();
    let walk_expressions = Arc::clone(&expressions);

    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = scan::make_searcher();
            let tx = tx.clone();
            let pre_filter = pre_filter.clone();
            let expressions = Arc::clone(&walk_expressions);
            Box::new(move |result| {
                let dirent = match result {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("Warning: {e}");
                        return WalkState::Continue;
                    }
                };
                if dirent.file_type().is_none_or(|ft| !ft.is_file()) {
                    return WalkState::Continue;
                }
                let path = dirent.path();
                if !scan::is_candidate_path(path) {
                    return WalkState::Continue;
                }
                let listed = if filter_by_change {
                    let Some(contents) =
                        scan::file_contents_if_matches(&mut searcher, &pre_filter, path)
                    else {
                        return WalkState::Continue;
                    };
                    let (updated, count, _) =
                        apply_compiled_expressions(&contents, &expressions, false);
                    count > 0 && *updated != *contents
                } else {
                    scan::file_matches(&mut searcher, &pre_filter, path)
                };
                if listed && tx.send(display_path(path)).is_err() {
                    return WalkState::Quit;
                }
                WalkState::Continue
            })
        });
    });

    let mut paths: Vec<String> = rx.iter().collect();
    paths.sort_by(|a, b| natord::compare(a, b));
    for path in &paths {
        println!("{path}");
    }
    Ok(())
}

fn run_walk_and_apply(cli: &Cli, write: bool) -> Result<()> {
    use std::sync::Arc;
    use std::sync::mpsc::channel;
    use std::thread;

    use ignore::WalkState;

    let expressions = Arc::new(compile_expressions(cli)?);
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;

    let stdout_terminal = std::io::stdout().is_terminal();
    let force_color = ui::color_choice() == ui::ColorChoice::Always;
    let will_render_color = stdout_terminal || force_color;
    let hyperlink_format = cli.hyperlink_format.as_deref().and_then(hyperlink_format);
    // Span tracking pays for itself when (a) hyperlinks need a per-line
    // first-column for `{column}` substitution, or (b) we'll render an inline
    // diff and want span-driven highlighting. Spans from chained expressions
    // are not valid against the final output buffer (later expressions shift
    // earlier ones' offsets), so single-expression runs are the only ones
    // that get inline highlighting from spans.
    let needs_first_column = hyperlink_format
        .as_deref()
        .is_some_and(hyperlink_format_uses_column);
    let render_inline_diff = will_render_color && !cli.quiet && expressions.len() == 1;
    let track_spans = (stdout_terminal && !cli.quiet && needs_first_column) || render_inline_diff;
    let linewise_diff = will_render_color
        && !cli.quiet
        && expressions
            .iter()
            .all(|expr| expr.preserves_line_boundaries);
    let multiline_span_diff = render_inline_diff
        && !cli.regexp
        && !cli.dotall
        && !cli.ignore_case
        && !cli.greedy
        && !cli.word_regexp
        && !cli.line_regexp
        && !cli.delete
        && expressions
            .iter()
            .any(|expr| !expr.preserves_line_boundaries);

    let dirs = cli.dirs();
    let mut builder = scan::walk_builder_with_file_set(&dirs, cli.file_set())?;
    scan::apply_walk_flags(&mut builder, cli.hidden, cli.no_ignore);
    let walk = builder
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        ))
        .build_parallel();

    let (tx, rx) = channel::<Result<ReplacementResult>>();
    let walk_expressions = Arc::clone(&expressions);

    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = scan::make_searcher();
            let tx = tx.clone();
            let expressions = Arc::clone(&walk_expressions);
            let pre_filter = pre_filter.clone();
            Box::new(move |result| {
                let dirent = match result {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("Warning: {e}");
                        return WalkState::Continue;
                    }
                };
                if dirent.file_type().is_none_or(|ft| !ft.is_file()) {
                    return WalkState::Continue;
                }
                let path = dirent.path();
                if !scan::is_candidate_path(path) {
                    return WalkState::Continue;
                }
                let Some(contents) =
                    scan::file_contents_if_matches(&mut searcher, &pre_filter, path)
                else {
                    return WalkState::Continue;
                };
                let (updated, count, spans) =
                    apply_compiled_expressions(&contents, &expressions, track_spans);
                if count == 0 {
                    return WalkState::Continue;
                }
                let columns = first_column_map_if_needed(needs_first_column, &contents, &spans);
                let diff = match (
                    String::from_utf8(contents.clone()),
                    String::from_utf8(updated.as_ref().to_vec()),
                ) {
                    (Ok(old), Ok(new)) => Some((old, new)),
                    _ => {
                        if !write {
                            eprintln!(
                                "Warning: {}: skipping diff (not valid UTF-8; use non-dry-run mode)",
                                path.display()
                            );
                        }
                        None
                    }
                };
                let payload = if write && let Err(e) = std::fs::write(path, &*updated) {
                    Err(anyhow::Error::new(e).context(format!("Unable to write to {path:?}")))
                } else {
                    Ok(ReplacementResult {
                        path: display_path(path),
                        link_path: hyperlink_path(path),
                        count,
                        diff,
                        columns,
                        spans: if render_inline_diff { spans } else { Vec::new() },
                        linewise_diff,
                        multiline_span_diff,
                    })
                };
                if tx.send(payload).is_err() {
                    return WalkState::Quit;
                }
                WalkState::Continue
            })
        });
    });

    let mut ok_results = Vec::new();
    while let Ok(result) = rx.recv() {
        ok_results.push(result?);
    }

    ok_results.sort_by(|a, b| natord::compare(&a.path, &b.path));
    ResultPrinter {
        quiet: cli.quiet,
        delete: cli.delete,
        dry: !write,
        no_hints: cli.no_hints,
        hyperlink_format: hyperlink_format.as_deref(),
        hyperlink_limit: cli.hyperlink_limit,
        context_lines: cli.context,
    }
    .print(&ok_results);
    Ok(())
}

fn run_preview(cli: &Cli) -> Result<()> {
    let expressions = compile_expressions(cli)?;
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;
    let expr_refs: Vec<interactive::PreviewExpr<'_>> = expressions
        .iter()
        .map(CompiledExpression::preview_expr)
        .collect();
    let mut fm = interactive::InteractivePatcher::new(false, cli.preview_tool(), cli.context);
    let dirs = cli.dirs();
    for (path, contents) in scan::matching_files_parallel(
        &dirs,
        cli.file_set(),
        cli.hidden,
        cli.no_ignore,
        &pre_filter,
    )? {
        // Preview mode relies on char-boundary arithmetic in the TUI, so
        // coerce to a `String` here. Files whose bytes are not valid UTF-8
        // are skipped - the non-preview apply path operates on bytes and
        // handles them faithfully; interactive preview can't.
        let Ok(contents) = String::from_utf8(contents) else {
            eprintln!(
                "Warning: {}: skipping (not valid UTF-8; use non-preview mode)",
                path.display()
            );
            continue;
        };
        fm.present_and_apply_patches_multi(&expr_refs, &path, contents)?;
    }
    Ok(())
}

fn run_stdin(cli: &Cli) -> Result<()> {
    use std::io::{self, Read as _, Write as _};
    let expressions = compile_expressions(cli)?;
    let mut input = Vec::new();
    io::stdin().lock().read_to_end(&mut input)?;
    let (output, _, _) = apply_compiled_expressions(&input, &expressions, false);
    io::stdout().lock().write_all(&output)?;
    Ok(())
}

/// Render `n` using the system locale's thousands separator (e.g. `648098` -> `648,098`
/// on `en_US`, `648.098` on `de_DE`). Locales whose separator is whitespace (`fr_FR`'s NBSP,
/// `sv_SE`'s regular space, etc.) fall back to `,` because a space inside a count is
/// ambiguous in CLI output - it reads as a word boundary, not a digit group. Same
/// fallback when the system locale cannot be read at all.
fn format_count<F>(n: usize, format: &F) -> String
where
    F: num_format::Format,
{
    use num_format::ToFormattedString as _;
    n.to_formatted_string(format)
}

fn has_ambiguous_digit_group_separator(separator: &str) -> bool {
    separator.chars().all(char::is_whitespace)
}

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

/// Renders replacement results, dispatching between a colored terminal view
/// and a plain unified-diff patch view based on whether stdout is a TTY.
///
/// `dry=true` -> yellow "Would perform"; `dry=false` -> green "Performed".
/// Write + `quiet` -> silence all output. Dry-run + `quiet` -> suppress diff only.
struct ResultPrinter<'a> {
    quiet: bool,
    delete: bool,
    dry: bool,
    no_hints: bool,
    hyperlink_format: Option<&'a str>,
    hyperlink_limit: u64,
    context_lines: usize,
}

impl ResultPrinter<'_> {
    fn print(&self, results: &[ReplacementResult]) {
        if !self.dry && self.quiet {
            return;
        }

        let stdout_is_terminal = std::io::stdout().is_terminal();
        let force_color = ui::color_choice() == ui::ColorChoice::Always;
        if !stdout_is_terminal && !force_color {
            if !self.quiet {
                self.print_patch_results(results);
            }
            return;
        }

        let total_files = results.len();
        let total_matches: usize = results.iter().map(|result| result.count).sum();
        let styles = Styles::when(true);
        // When the match count blows past the configured limit, the per-line
        // OSC 8 sequences become a tax on the terminal (parsing, scrollback
        // tracking) without any practical benefit - users can't click through
        // thousands of links anyway. A limit of 0 means "always render".
        let hyperlinks_disabled_by_limit = self.hyperlink_limit > 0
            && total_matches > usize::try_from(self.hyperlink_limit).unwrap_or(usize::MAX);
        let effective_format = if hyperlinks_disabled_by_limit {
            None
        } else {
            self.hyperlink_format
        };
        let template = effective_format.map(HyperlinkTemplate::parse);
        for (idx, result) in results.iter().enumerate() {
            let count = with_commas(result.count);
            let encoded_path = template
                .as_ref()
                .filter(|t| t.uses_path())
                .map_or(String::new(), |_| percent_encode_path(&result.link_path));
            let path = hyperlink_with_template(template.as_ref(), &encoded_path, 0, &result.path);
            println!(
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
                    template.as_ref(),
                    &encoded_path,
                    &result.columns,
                );
            }

            if !self.quiet && idx + 1 < results.len() {
                println!();
            }
        }

        if total_files > 0 {
            let color = if self.dry {
                Color::Yellow
            } else {
                Color::Green
            };
            let msg = summary_message(total_files, total_matches, self.delete, self.dry);
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
            println!(
                "\n{}{}{}{}{hint}",
                styles.bold(),
                styles.fg(color),
                msg,
                styles.reset()
            );
        }
    }

    fn print_patch_results(&self, results: &[ReplacementResult]) {
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
            print!("{patch}");
        }
    }
}

fn print_error(err: &anyhow::Error) {
    let styles = Styles::when(std::io::stderr().is_terminal());
    eprintln!(
        "{}{}error:{} {err}",
        styles.bold(),
        styles.fg(Color::Red),
        styles.reset()
    );
}

fn main() {
    if let Err(err) = run() {
        print_error(&err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut argv: Vec<_> = std::env::args().collect();
    let rc_args = config::rc_args();
    if !rc_args.is_empty() {
        // Insert rc args after argv[0] so clap sees: [program, ...rc, ...cli].
        // CLI args come last so they win for `Option<T>` (last occurrence) and
        // positionals stay in their expected positions.
        let tail = argv.split_off(1);
        argv.extend(
            rc_args
                .into_iter()
                .map(|a| a.into_string().unwrap_or_default()),
        );
        argv.extend(tail);
    }
    let cli = Cli::parse_from(preprocess_expression_args(argv));
    ui::set_color_choice(cli.color);
    let theme = theme::Theme::from_overrides(theme::Overrides {
        style_added: cli.style_added.as_deref(),
        style_removed: cli.style_removed.as_deref(),
        style_line_added: cli.style_line_added.as_deref(),
        style_line_removed: cli.style_line_removed.as_deref(),
        marker_added: cli.marker_added.clone(),
        marker_removed: cli.marker_removed.clone(),
    })
    .map_err(|e| anyhow::anyhow!("invalid style: {e}"))?;
    theme::set_theme(theme);

    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "rep", &mut std::io::stdout());
        return Ok(());
    }

    if cli.help_long {
        print_help_long();
        std::process::exit(0);
    }

    if cli.help {
        print_help();
        std::process::exit(0);
    }

    if !cli.uses_expressions() && cli.args.is_empty() && !cli.delete && !cli.list_files {
        print_help();
        std::process::exit(1);
    }

    if cli.positional_skip() > cli.args.len() {
        let missing = if cli.is_find_only() || cli.args.is_empty() {
            "<find>"
        } else {
            "<replace>"
        };
        print_error(&anyhow::anyhow!("missing required argument: {missing}"));
        print_help();
        std::process::exit(1);
    }

    let paths = cli.paths();
    let has_stdin_arg = !cli.list_files && paths.iter().any(|p| p.to_str() == Some("-"));

    if has_stdin_arg && paths.len() > 1 {
        bail!("Cannot mix `-` (stdin) with other paths");
    }

    // Validate paths exist
    for dir in &cli.dirs() {
        if has_stdin_arg && *dir == "-" {
            continue;
        }
        if !std::path::Path::new(dir).exists() {
            bail!("no such file or directory {dir:?}");
        }
    }

    if cli.list_files {
        return run_list_files(&cli);
    }

    let is_stdin_mode = has_stdin_arg || (paths.is_empty() && stdin_has_input());

    if cli.smart && paths.len() > 1 {
        bail!("Smart mode only supports a single path");
    }

    if cli.preview && !std::io::stdin().is_terminal() {
        bail!("--preview requires an interactive terminal");
    }

    if is_stdin_mode {
        run_stdin(&cli)
    } else if cli.write {
        run_walk_and_apply(&cli, true)
    } else if cli.preview {
        run_preview(&cli)
    } else {
        run_walk_and_apply(&cli, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let processed =
            preprocess_expression_args(args.iter().map(std::string::ToString::to_string).collect());
        Cli::parse_from(processed)
    }

    #[test]
    fn test_parse_file_globs_extension() {
        assert_eq!(parse_file_globs("txt"), vec!["*.txt"]);
        assert_eq!(parse_file_globs("rs,go"), vec!["*.rs", "*.go"]);
    }

    #[test]
    fn test_parse_file_globs_exact_filename() {
        assert_eq!(parse_file_globs("=Dockerfile"), vec!["Dockerfile"]);
    }

    #[test]
    fn test_parse_file_globs_negation() {
        assert_eq!(parse_file_globs("!rs"), vec!["!*.rs"]);
        assert_eq!(parse_file_globs("!=Makefile"), vec!["!Makefile"]);
    }

    #[test]
    fn test_parse_file_globs_wildcard() {
        assert_eq!(parse_file_globs("*.json"), vec!["*.json"]);
    }

    #[test]
    fn test_parse_file_globs_dot_ignored() {
        assert!(parse_file_globs(".").is_empty());
    }

    #[test]
    fn test_parse_file_globs_mixed() {
        assert_eq!(
            parse_file_globs("rs, =Dockerfile, !txt"),
            vec!["*.rs", "Dockerfile", "!*.txt"]
        );
    }

    #[test]
    fn test_write_overrides_dry_run_last_wins() {
        let cli = parse_cli(&["rep", "--dry-run", "-W", "a", "b"]);
        assert!(!cli.dry_run);
        assert!(cli.write);
        assert!(!cli.preview);
    }

    #[test]
    fn test_dry_run_overrides_write_last_wins() {
        let cli = parse_cli(&["rep", "-W", "--dry-run", "a", "b"]);
        assert!(cli.dry_run);
        assert!(!cli.write);
    }

    #[test]
    fn test_preview_overrides_dry_run_last_wins() {
        let cli = parse_cli(&["rep", "--dry-run", "--preview", "a", "b"]);
        assert!(!cli.dry_run);
        assert!(cli.preview);
    }

    #[test]
    fn test_dry_run_overrides_preview_last_wins() {
        let cli = parse_cli(&["rep", "--preview", "--dry-run", "a", "b"]);
        assert!(cli.dry_run);
        assert!(!cli.preview);
    }

    #[test]
    fn test_expression_mode_without_paths_defaults_to_current_dir() {
        let cli = parse_cli(&["rep", "-e", "a", "b", "-e", "b", "c", "--dry-run"]);

        assert!(cli.paths().is_empty());
        assert_eq!(cli.dirs(), vec!["."]);
    }

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

    #[test]
    fn test_cli_is_regex_any_flag_enables_regex() {
        assert!(!Cli::parse_from(["rep", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-r", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-i", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-w", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-x", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-m", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "-G", "a", "b"]).is_regex());
        assert!(Cli::parse_from(["rep", "--dotall", "a", "b"]).is_regex());
    }

    #[test]
    fn test_cli_positional_skip() {
        // find+replace mode: skip 2 positional args
        assert_eq!(Cli::parse_from(["rep", "a", "b"]).positional_skip(), 2);
        // expression mode: no positional find/replace
        assert_eq!(parse_cli(&["rep", "-e", "a", "b"]).positional_skip(), 0);
        // -l accepts an optional <replace>: 1 positional stays find-only,
        // 2+ positionals consume both find and replace.
        assert_eq!(Cli::parse_from(["rep", "-l", "a"]).positional_skip(), 1);
        assert_eq!(
            Cli::parse_from(["rep", "-l", "a", "b"]).positional_skip(),
            2
        );
        assert_eq!(
            Cli::parse_from(["rep", "-l", "a", "b", "src"]).positional_skip(),
            2
        );
        // -d -l keeps delete semantics (no <replace>).
        assert_eq!(
            Cli::parse_from(["rep", "-d", "-l", "a", "src"]).positional_skip(),
            1
        );
    }

    #[test]
    fn test_cli_is_find_only() {
        // -l is no longer find-only: it accepts an optional <replace>.
        assert!(!Cli::parse_from(["rep", "-l", "a"]).is_find_only());
        assert!(!Cli::parse_from(["rep", "-l", "a", "b"]).is_find_only());
        assert!(!Cli::parse_from(["rep", "a", "b"]).is_find_only());
        // -l with -e is expression mode, not find-only
        assert!(!parse_cli(&["rep", "-l", "-e", "a", "b"]).is_find_only());
        // -d is always find-only regardless of trailing positional path count
        assert!(Cli::parse_from(["rep", "-d", "a"]).is_find_only());
        assert!(Cli::parse_from(["rep", "-d", "a", "src"]).is_find_only());
        assert!(Cli::parse_from(["rep", "-d", "a", "src", "tests"]).is_find_only());
        // -d -l keeps delete's find-only semantics.
        assert!(Cli::parse_from(["rep", "-d", "-l", "a", "src"]).is_find_only());
    }

    #[test]
    fn test_delete_mode_treats_trailing_positionals_as_paths() {
        // With -d, there is no <replace>; args[1..] are all paths.
        let cli = Cli::parse_from(["rep", "-d", "TODO", "src", "tests"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_list_files_mode_consumes_optional_replace() {
        // 1 positional: find-only, default search root.
        let cli = Cli::parse_from(["rep", "-l", "TODO"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(cli.paths(), Vec::<PathBuf>::new());

        // 2 positionals: <find> <replace>, default search root.
        let cli = Cli::parse_from(["rep", "-l", "foo", "bar"]);
        assert_eq!(cli.positional_skip(), 2);
        assert_eq!(cli.pattern(), "foo");
        assert_eq!(cli.replacement(), "bar");
        assert_eq!(cli.paths(), Vec::<PathBuf>::new());

        // 3+ positionals: <find> <replace> followed by paths.
        let cli = Cli::parse_from(["rep", "-l", "foo", "bar", "src", "tests"]);
        assert_eq!(cli.positional_skip(), 2);
        assert_eq!(cli.pattern(), "foo");
        assert_eq!(cli.replacement(), "bar");
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_delete_list_files_mode_treats_trailing_positionals_as_paths() {
        // -d -l keeps -d's parsing: no <replace>, all trailing positionals are paths.
        let cli = Cli::parse_from(["rep", "-d", "-l", "TODO", "src", "tests"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_delete_combines_with_smart_flag() {
        let cli = Cli::parse_from(["rep", "-d", "-S", "foo_bar"]);
        assert!(cli.delete);
        assert!(cli.smart);
    }

    #[test]
    fn test_delete_combines_with_list_files() {
        let cli = Cli::parse_from(["rep", "-d", "-l", "foo"]);
        assert!(cli.delete);
        assert!(cli.list_files);
    }

    #[test]
    fn test_color_flag_parses_all_variants() {
        assert_eq!(
            Cli::parse_from(["rep", "--color=auto", "a", "b"]).color,
            ColorChoice::Auto
        );
        assert_eq!(
            Cli::parse_from(["rep", "--color=always", "a", "b"]).color,
            ColorChoice::Always
        );
        assert_eq!(
            Cli::parse_from(["rep", "--color=never", "a", "b"]).color,
            ColorChoice::Never
        );
        // Default when omitted.
        assert_eq!(Cli::parse_from(["rep", "a", "b"]).color, ColorChoice::Auto);
    }

    #[test]
    fn test_color_flag_rejects_invalid_value() {
        assert!(Cli::try_parse_from(["rep", "--color=bogus", "a", "b"]).is_err());
    }

    #[test]
    fn test_preview_tool_requires_preview() {
        let result = Cli::try_parse_from(["rep", "--preview-tool", "delta", "foo", "bar"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_dirs_defaults_to_current_directory() {
        assert_eq!(Cli::parse_from(["rep", "a", "b"]).dirs(), vec!["."]);
    }

    #[test]
    fn test_cli_dirs_uses_trailing_positionals() {
        let cli = Cli::parse_from(["rep", "a", "b", "src", "tests"]);
        assert_eq!(cli.dirs(), vec!["src", "tests"]);
    }

    #[test]
    fn test_cli_paths_skips_find_and_replace() {
        let cli = Cli::parse_from(["rep", "a", "b", "src", "tests"]);
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_parse_file_globs_empty_string_is_empty() {
        assert!(parse_file_globs("").is_empty());
    }

    #[test]
    fn test_parse_file_globs_only_commas_is_empty() {
        assert!(parse_file_globs(",,,").is_empty());
    }

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

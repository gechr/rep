mod expressions;
mod interactive;
mod scan;
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
    CompiledExpression, EXPR_SEP, apply_compiled_expressions, build_pre_filter_matcher,
    compile_expressions,
};
use crate::ui::Color;
use crate::ui::Styles;

struct ReplacementResult {
    path: String,
    link_path: String,
    count: usize,
    diff: Option<(String, String)>,
}

#[derive(Parser)]
#[command(name = "rep", version, disable_help_flag = true)]
struct Cli {
    #[arg(value_name = "arg")]
    args: Vec<String>,

    #[arg(short = 'h', help = "Print help")]
    help: bool,

    #[arg(long = "help", hide = true)]
    help_long: bool,

    /// File glob patterns
    #[arg(short = 'f', long = "files")]
    files: Option<String>,

    /// Include hidden files
    #[arg(short = 'H', long = "hidden")]
    hidden: bool,

    /// Ignore .gitignore / .ignore / .git/info/exclude
    #[arg(long = "no-ignore")]
    no_ignore: bool,

    /// Greedy matching
    #[arg(short = 'G', long = "greedy")]
    greedy: bool,

    /// Case-insensitive
    #[arg(short = 'i', long = "ignore-case")]
    ignore_case: bool,

    /// Multiline matching
    #[arg(short = 'm', long = "multiline")]
    multiline: bool,

    /// Dot matches newlines
    #[arg(long = "dotall")]
    dotall: bool,

    /// Use regex
    #[arg(short = 'r', long = "regex", alias = "regexp")]
    regexp: bool,

    /// Preserve-case replacement
    #[arg(short = 'S', long = "smart")]
    smart: bool,

    /// Find replace expression
    #[arg(short = 'e', long = "expression", value_name = "<find> <replace>")]
    expressions: Vec<String>,

    /// Whole words only
    #[arg(short = 'w', long = "word-regexp")]
    word_regexp: bool,

    /// Match only whole lines
    #[arg(short = 'x', long = "line-regexp")]
    line_regexp: bool,

    /// Print matching file paths
    #[arg(short = 'l', long = "list-files")]
    list_files: bool,

    /// Delete lines matching <find>
    #[arg(short = 'd', long = "delete")]
    delete: bool,

    /// Dry run
    #[arg(
        short = 'n',
        long = "dry-run",
        alias = "dry",
        conflicts_with = "preview"
    )]
    dry_run: bool,

    /// Interactive preview
    #[arg(short = 'p', long = "preview")]
    preview: bool,

    /// Diff tool for preview
    #[arg(long = "preview-tool", requires = "preview")]
    preview_tool: Option<String>,

    /// Suppress final summary output
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    #[arg(long = "completions", value_name = "SHELL", hide = true)]
    completions: Option<Shell>,
}

fn print_help() {
    let styles = ui::Styles::when(std::io::stdout().is_terminal());
    let bold = styles.bold();
    let dim = styles.fg(Color::Dim);
    let red = styles.fg(Color::Red);
    let green = styles.fg(Color::Green);
    let yellow = styles.fg(Color::Yellow);
    let blue = styles.fg(Color::Blue);
    let magenta = styles.fg(Color::Magenta);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();

    let text = format!(
        "\
{yellow}{bold}Usage{reset}

  {green}{bold}rep{reset} {red}[options]{reset} {blue}<find> <replace>{reset} {magenta}[<path>…]{reset}

    {blue}<find>{reset}     String to find
    {blue}<replace>{reset}  String to replace with
    {magenta}<path>…{reset}    Paths to search in {grey}(optional){reset}

{yellow}{bold}Filter{reset}

  {red}-f{reset}, {red}--files {dim}<glob>{reset}        Smart glob patterns to match files against
  {red}-H{reset}, {red}--hidden{reset}              Search hidden files and directories

{yellow}{bold}Replace{reset}

  {red}-e{reset}, {red}--expression {dim}<f> <r>{reset}  Find/replace expression
  {red}-S{reset}, {red}--smart{reset}               Replace all case variants of the pattern

{yellow}{bold}Regex{reset}

  {red}-G{reset}, {red}--greedy{reset}              Use greedy matching for regular expressions
  {red}-i{reset}, {red}--ignore-case{reset}         Case-insensitive matching
  {red}-m{reset}, {red}--multiline{reset}           Search across multiple lines
      {red}--dotall{reset}              Allow dot to match newlines
  {red}-r{reset}, {red}--regex{reset}               Treat patterns as regular expressions
  {red}-w{reset}, {red}--word-regexp{reset}         Match only whole words
  {red}-x{reset}, {red}--line-regexp{reset}         Match only whole lines

{yellow}{bold}Behavior{reset}

  {red}-d{reset}, {red}--delete{reset}              Delete lines matching {blue}<find>{reset}
  {red}-l{reset}, {red}--list-files{reset}          Print only file paths that contain matches

  {red}-n{reset}, {red}--dry-run{reset}             Show what would be changed without writing
  {red}-p{reset}, {red}--preview{reset}             Preview the changes before applying them
      {red}--preview-tool {dim}<cmd>{reset}  External diff tool for preview mode

{yellow}{bold}Miscellaneous{reset}

  {red}-q{reset}, {red}--quiet{reset}               Suppress summary output
  {red}-V{reset}, {red}--version{reset}             Print version

  {red}-h{reset}                        Print short help
      {red}--help{reset}                Print long help with examples
"
    );
    print!("{text}");
}

fn print_help_long() {
    let styles = ui::Styles::when(std::io::stdout().is_terminal());
    let bold = styles.bold();
    let green = styles.fg(Color::Green);
    let yellow = styles.fg(Color::Yellow);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();

    print_help();

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
    /// Fill in defaults from `REP_*` env vars. CLI flags take precedence:
    /// for booleans, an explicit `--flag` (true) is never overridden; for
    /// `Option<T>`, env only fills when the flag is absent (`None`).
    fn apply_env_defaults(&mut self) {
        self.apply_env_defaults_with(|k| std::env::var(k).ok());
    }

    /// Testable core of `apply_env_defaults`. Skips env fallback where it would
    /// violate a clap-level conflict that the user's CLI flags already expressed
    /// (e.g. `-d` vs `REP_SMART`).
    fn apply_env_defaults_with(&mut self, get: impl Fn(&str) -> Option<String>) {
        // Truthy: "1", "true" (case-insensitive). Anything else is false.
        let bool_var = |k| {
            get(k)
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true"))
                .unwrap_or(false)
        };
        let str_var = |k| get(k).filter(|v| !v.is_empty());

        self.hidden |= bool_var("REP_HIDDEN");
        self.no_ignore |= bool_var("REP_NO_IGNORE");
        self.greedy |= bool_var("REP_GREEDY");
        self.ignore_case |= bool_var("REP_IGNORE_CASE");
        self.regexp |= bool_var("REP_REGEXP");

        self.smart |= bool_var("REP_SMART");
        // `preview` conflicts with `dry_run`; don't let env re-enable preview on a dry-run.
        if !self.dry_run {
            self.preview |= bool_var("REP_PREVIEW");
        }

        if self.preview_tool.is_none() {
            self.preview_tool = str_var("REP_PREVIEW_TOOL");
        }
    }

    fn uses_expressions(&self) -> bool {
        !self.expressions.is_empty()
    }

    /// True when the CLI takes only `<find>` (no `<replace>`).
    ///
    /// - `-d`/`--delete`: replacement is forbidden; trailing positionals are paths.
    /// - `-l`/`--list-files` without `-e`: consumes only `<find>`; all remaining
    ///   positionals are search roots.
    fn is_find_only(&self) -> bool {
        !self.uses_expressions() && (self.delete || self.list_files)
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

    fn is_regex(&self) -> bool {
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
        } else {
            2
        }
    }

    fn dirs(&self) -> Vec<&str> {
        let args = &self.args[self.positional_skip()..];
        if args.is_empty() {
            vec!["."]
        } else {
            args.iter().map(|arg| arg.as_str()).collect()
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
    let mut iter = args.into_iter().peekable();
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
            // Compact form: -efoo → find="foo", next arg is replace
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
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    abs.to_string_lossy().to_string()
}

fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

fn hyperlink_format(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "cursor" => String::from("cursor://file{path}:{line}"),
        "vscode" => String::from("vscode://file{path}:{line}"),
        "vscode-insiders" => String::from("vscode-insiders://file{path}:{line}"),
        "vscodium" => String::from("vscodium://file{path}:{line}"),
        _ => value.to_string(),
    }
}

fn hyperlink_url(format: &str, path: &str, line: usize) -> String {
    let url = format.replace("{path}", path);
    if line > 0 {
        return url.replace("{line}", &line.to_string());
    }

    url.replace(":{line}", "")
        .replace("#{line}", "")
        .replace("&line={line}", "")
        .replace("?line={line}", "")
        .replace("{line}", "")
}

fn hyperlink(format: Option<&str>, path: &str, line: usize, text: &str) -> String {
    format.map_or_else(
        || text.to_string(),
        |format| osc8(&hyperlink_url(format, path, line), text),
    )
}

/// Parse the `-f` smart glob mini-DSL into the iglob patterns consumed
/// by `scan::walk_builder_with_file_set`.
///
/// Supports comma-separated patterns:
///   `txt`         → `*.txt`        (extension)
///   `=Dockerfile` → `Dockerfile`   (exact filename)
///   `!=Makefile`  → `!Makefile`    (exclude exact filename)
///   `*.json`      → `*.json`       (glob as-is)
///   `!rs`         → `!*.rs`        (exclude extension)
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

    let expressions = compile_expressions(cli)?;
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;

    let mut builder = scan::walk_builder_with_file_set(cli.dirs(), cli.file_set())?;
    scan::apply_walk_flags(&mut builder, cli.hidden, cli.no_ignore);
    let walk = builder
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
        ))
        .build_parallel();

    let (tx, rx) = channel::<String>();

    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = scan::make_searcher();
            let tx = tx.clone();
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
                if scan::file_matches(&mut searcher, &pre_filter, path)
                    && tx.send(display_path(path)).is_err()
                {
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

    let mut builder = scan::walk_builder_with_file_set(cli.dirs(), cli.file_set())?;
    scan::apply_walk_flags(&mut builder, cli.hidden, cli.no_ignore);
    let walk = builder
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
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
                let (updated, count) = apply_compiled_expressions(&contents, &expressions);
                if count == 0 {
                    return WalkState::Continue;
                }
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
    print_results(&ok_results, !write, cli.quiet);
    Ok(())
}

fn run_preview(cli: &Cli) -> Result<()> {
    let expressions = compile_expressions(cli)?;
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;
    let expr_refs: Vec<interactive::PreviewExpr<'_>> = expressions
        .iter()
        .map(CompiledExpression::preview_expr)
        .collect();
    let mut fm = interactive::InteractivePatcher::new(false, cli.preview_tool());
    for (path, contents) in scan::matching_files_parallel(
        cli.dirs(),
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
    let (output, _) = apply_compiled_expressions(&input, &expressions);
    io::stdout().lock().write_all(&output)?;
    Ok(())
}

/// Render `n` using the system locale's thousands separator (e.g. `648098` → `648,098`
/// on en_US, `648.098` on de_DE). Locales whose separator is whitespace (fr_FR's NBSP,
/// sv_SE's regular space, etc.) fall back to `,` because a space inside a count is
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
    let fallback = || format_count(n, &num_format::Locale::en);
    let Ok(loc) = num_format::SystemLocale::default() else {
        return fallback();
    };
    if has_ambiguous_digit_group_separator(loc.separator()) {
        return fallback();
    }
    format_count(n, &loc)
}

fn summary_message_with_formatter<F>(
    total_files: usize,
    total_matches: usize,
    dry: bool,
    format_count: F,
) -> String
where
    F: Fn(usize) -> String,
{
    let verb = if dry { "Would perform" } else { "Performed" };
    format!(
        "{} {} replacement{} in {} file{}",
        verb,
        format_count(total_matches),
        if total_matches == 1 { "" } else { "s" },
        format_count(total_files),
        if total_files == 1 { "" } else { "s" },
    )
}

fn summary_message(total_files: usize, total_matches: usize, dry: bool) -> String {
    summary_message_with_formatter(total_files, total_matches, dry, with_commas)
}

/// `dry=true` → yellow "Would perform"; `dry=false` → green "Performed".
/// Write + `quiet` → silence all output. Dry-run + `quiet` → suppress diff only.
fn print_results(results: &[ReplacementResult], dry: bool, quiet: bool) {
    if !dry && quiet {
        return;
    }

    let stdout_is_terminal = std::io::stdout().is_terminal();
    if !stdout_is_terminal {
        if !quiet {
            print_patch_results(results);
        }
        return;
    }

    let total_files = results.len();
    let total_matches: usize = results.iter().map(|result| result.count).sum();
    let styles = Styles::ansi();
    let hyperlink_format = std::env::var("REP_HYPERLINK_FORMAT")
        .ok()
        .map(|value| hyperlink_format(&value));

    for (idx, result) in results.iter().enumerate() {
        let count = with_commas(result.count);
        let path = hyperlink(
            hyperlink_format.as_deref(),
            &result.link_path,
            0,
            &result.path,
        );
        println!(
            "{}{} {}({count}){}",
            if quiet { "" } else { styles.fg(Color::Magenta) },
            path,
            styles.fg(Color::Grey),
            styles.reset()
        );

        if !quiet && let Some((old, new)) = &result.diff {
            interactive::print_file_line_diff(
                old,
                new,
                styles,
                hyperlink_format.as_deref(),
                &result.link_path,
            );
        }

        if !quiet && idx + 1 < results.len() {
            println!();
        }
    }

    if total_files > 0 {
        let color = if dry { Color::Yellow } else { Color::Green };
        let msg = summary_message(total_files, total_matches, dry);
        println!(
            "\n{}{}{}{}",
            styles.bold(),
            styles.fg(color),
            msg,
            styles.reset()
        );
    }
}

fn print_patch_results(results: &[ReplacementResult]) {
    for result in results {
        let Some((old, new)) = &result.diff else {
            continue;
        };
        let mut options = DiffOptions::new();
        options
            .set_original_filename(format!("a/{}", result.path))
            .set_modified_filename(format!("b/{}", result.path));
        let patch = options.create_patch(old, new);
        print!("{patch}");
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
    let mut cli = Cli::parse_from(preprocess_expression_args(std::env::args().collect()));
    cli.apply_env_defaults();

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
            bail!("{dir}: no such file or directory");
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
    } else if cli.dry_run {
        run_walk_and_apply(&cli, false)
    } else if cli.preview {
        run_preview(&cli)
    } else {
        run_walk_and_apply(&cli, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let processed = preprocess_expression_args(args.iter().map(|s| s.to_string()).collect());
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
            hyperlink_url("vscode://file{path}:{line}", "/tmp/a.txt", 42),
            "vscode://file/tmp/a.txt:42"
        );
    }

    #[test]
    fn test_hyperlink_format_expands_presets() {
        assert_eq!(hyperlink_format("vscode"), "vscode://file{path}:{line}");
        assert_eq!(hyperlink_format("cursor"), "cursor://file{path}:{line}");
        assert_eq!(
            hyperlink_format("custom://open/{path}:{line}"),
            "custom://open/{path}:{line}"
        );
    }

    #[test]
    fn test_hyperlink_url_omits_zero_line() {
        assert_eq!(
            hyperlink_url("vscode://file{path}:{line}", "/tmp/a.txt", 0),
            "vscode://file/tmp/a.txt"
        );
        assert_eq!(
            hyperlink_url("idea://open?file={path}&line={line}", "/tmp/a.txt", 0),
            "idea://open?file=/tmp/a.txt"
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
        // -l always consumes only the find pattern.
        assert_eq!(Cli::parse_from(["rep", "-l", "a"]).positional_skip(), 1);
        assert_eq!(
            Cli::parse_from(["rep", "-l", "a", "b"]).positional_skip(),
            1
        );
    }

    #[test]
    fn test_cli_is_find_only() {
        assert!(Cli::parse_from(["rep", "-l", "a"]).is_find_only());
        assert!(Cli::parse_from(["rep", "-l", "a", "b"]).is_find_only());
        assert!(!Cli::parse_from(["rep", "a", "b"]).is_find_only());
        // -l with -e is expression mode, not find-only
        assert!(!parse_cli(&["rep", "-l", "-e", "a", "b"]).is_find_only());
        // -d is always find-only regardless of trailing positional path count
        assert!(Cli::parse_from(["rep", "-d", "a"]).is_find_only());
        assert!(Cli::parse_from(["rep", "-d", "a", "src"]).is_find_only());
        assert!(Cli::parse_from(["rep", "-d", "a", "src", "tests"]).is_find_only());
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
    fn test_list_files_mode_treats_trailing_positionals_as_paths() {
        let cli = Cli::parse_from(["rep", "-l", "TODO", "src", "tests"]);
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
    fn test_env_defaults_enable_boolean_flags() {
        let env = std::collections::HashMap::from([
            ("REP_HIDDEN", "1"),
            ("REP_NO_IGNORE", "true"),
            ("REP_SMART", "TRUE"),
            ("REP_IGNORE_CASE", "1"),
            ("REP_GREEDY", "1"),
            ("REP_REGEXP", "1"),
            ("REP_PREVIEW", "1"),
            ("REP_PREVIEW_TOOL", "delta --side-by-side"),
        ]);
        let mut cli = Cli::parse_from(["rep", "foo", "bar"]);
        cli.apply_env_defaults_with(|k| env.get(k).map(|s| (*s).to_owned()));
        assert!(cli.hidden);
        assert!(cli.no_ignore);
        assert!(cli.smart);
        assert!(cli.ignore_case);
        assert!(cli.greedy);
        assert!(cli.regexp);
        assert!(cli.preview);
        assert_eq!(cli.preview_tool.as_deref(), Some("delta --side-by-side"));
    }

    #[test]
    fn test_env_defaults_falsy_values_are_ignored() {
        let env = std::collections::HashMap::from([
            ("REP_HIDDEN", "0"),
            ("REP_SMART", "false"),
            ("REP_PREVIEW_TOOL", ""),
        ]);
        let mut cli = Cli::parse_from(["rep", "foo", "bar"]);
        cli.apply_env_defaults_with(|k| env.get(k).map(|s| (*s).to_owned()));
        assert!(!cli.hidden);
        assert!(!cli.smart);
        assert!(cli.preview_tool.is_none());
    }

    #[test]
    fn test_cli_flag_wins_over_env_for_preview_tool() {
        let env = std::collections::HashMap::from([("REP_PREVIEW_TOOL", "delta")]);
        let mut cli = Cli::parse_from(["rep", "-p", "--preview-tool", "diff -u", "foo", "bar"]);
        cli.apply_env_defaults_with(|k| env.get(k).map(|s| (*s).to_owned()));
        assert_eq!(cli.preview_tool.as_deref(), Some("diff -u"));
    }

    #[test]
    fn test_env_preview_skipped_when_dry_run_flag_present() {
        let env = std::collections::HashMap::from([("REP_PREVIEW", "1")]);
        let mut cli = Cli::parse_from(["rep", "-n", "foo", "bar"]);
        cli.apply_env_defaults_with(|k| env.get(k).map(|s| (*s).to_owned()));
        assert!(!cli.preview);
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
            summary_message(1, 1, false),
            "Performed 1 replacement in 1 file"
        );
    }

    #[test]
    fn test_summary_message_plural() {
        assert_eq!(
            summary_message(2, 5, false),
            "Performed 5 replacements in 2 files"
        );
    }

    #[test]
    fn test_summary_message_dry_run_uses_would_perform() {
        assert_eq!(
            summary_message(1, 1, true),
            "Would perform 1 replacement in 1 file"
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
            summary_message_with_formatter(718, 648_098, false, |n| {
                format_count(n, &num_format::Locale::en)
            }),
            "Performed 648,098 replacements in 718 files"
        );
        assert_eq!(
            summary_message_with_formatter(1_000, 2_500_000, true, |n| {
                format_count(n, &num_format::Locale::en)
            }),
            "Would perform 2,500,000 replacements in 1,000 files"
        );
    }
}

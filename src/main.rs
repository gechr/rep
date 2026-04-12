mod fastmod;

use std::borrow::Cow;
use std::io::IsTerminal as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::{CommandFactory as _, Parser};
use clap_complete::Shell;
use grep::regex::RegexMatcherBuilder;
use regex::RegexBuilder;

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
    #[arg(short = 'r', long = "regexp")]
    regexp: bool,

    /// Preserve-case replacement
    #[arg(short = 'S', long = "smart")]
    smart: bool,

    /// Find=replace expression
    #[arg(short = 'e', long = "expression", value_name = "<find>=<replace>")]
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
    #[arg(
        short = 'd',
        long = "delete",
        conflicts_with_all = ["smart", "list_files"],
    )]
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
    #[arg(long = "diff-tool")]
    diff_tool: Option<String>,

    #[arg(long = "completions", value_name = "SHELL", hide = true)]
    completions: Option<Shell>,
}

fn print_help() {
    let is_tty = std::io::stderr().is_terminal();

    let (bold, dim, red, green, yellow, blue, magenta, _white, grey, reset) = if is_tty {
        (
            "\x1b[1m",
            "\x1b[2m",
            "\x1b[31m",
            "\x1b[32m",
            "\x1b[33m",
            "\x1b[34m",
            "\x1b[35m",
            "\x1b[37m",
            "\x1b[38;5;248m",
            "\x1b[m",
        )
    } else {
        ("", "", "", "", "", "", "", "", "", "")
    };

    eprint!(
        "\
{yellow}{bold}Usage{reset}

  {green}{bold}rep{reset} {red}[options]{reset} {blue}<find> <replace>{reset} {magenta}[<path>…]{reset}

    {blue}<find>{reset}     String to find
    {blue}<replace>{reset}  String to replace with
    {magenta}<path>…{reset}    Paths to search in {grey}(optional){reset}

{yellow}{bold}Filter{reset}

  {red}-f{reset}, {red}--files {dim}<glob>{reset}       Smart glob patterns to match files against
  {red}-H{reset}, {red}--hidden{reset}             Search hidden files and directories

{yellow}{bold}Replace{reset}

  {red}-e{reset}, {red}--expression {dim}<expr>{reset}  Replacement {blue}<find>{dim}={reset}{blue}<replace>{reset} expression
  {red}-S{reset}, {red}--smart{reset}              Replace all case variants of the pattern

{yellow}{bold}Regex{reset}

  {red}-G{reset}, {red}--greedy{reset}             Use greedy matching for regular expressions
  {red}-i{reset}, {red}--ignore-case{reset}        Case-insensitive matching
  {red}-m{reset}, {red}--multiline{reset}          Search across multiple lines
      {red}--dotall{reset}             Allow dot to match newlines
  {red}-r{reset}, {red}--regexp{reset}             Treat patterns as regular expressions
  {red}-w{reset}, {red}--word-regexp{reset}        Match only whole words
  {red}-x{reset}, {red}--line-regexp{reset}        Match only whole lines

{yellow}{bold}Behavior{reset}

  {red}-d{reset}, {red}--delete{reset}             Delete lines matching {blue}<find>{reset}
  {red}-l{reset}, {red}--list-files{reset}         Print only file paths that contain matches

  {red}-n{reset}, {red}--dry-run{reset}            Show what would be changed without writing
  {red}-p{reset}, {red}--preview{reset}            Preview the changes before applying them
"
    );
}

fn print_help_long() {
    let is_tty = std::io::stderr().is_terminal();

    let (bold, green, yellow, grey, reset) = if is_tty {
        (
            "\x1b[1m",
            "\x1b[32m",
            "\x1b[33m",
            "\x1b[38;5;248m",
            "\x1b[m",
        )
    } else {
        ("", "", "", "", "")
    };

    print_help();

    eprint!(
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
  {green}${reset} rep --regexp '[13]\\.2\\.[13]' 4.5.6

  {grey}# Swap \"foo.bar\" with \"bar.foo\" in all files{reset}
  {green}${reset} rep --regexp '(foo)\\.(bar)' '$2.$1'

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
  {green}${reset} rep -e foo=bar -e baz=qux src

  {grey}# Delete every line containing \"TODO\"{reset}
  {green}${reset} rep -d TODO
"
    );
}

impl Cli {
    fn uses_expressions(&self) -> bool {
        !self.expressions.is_empty()
    }

    /// True when the CLI takes only `<find>` (no `<replace>`).
    ///
    /// - `-d`/`--delete`: replacement is forbidden; trailing positionals are paths.
    /// - `-l`/`--list-files` without `-e`: replacement is optional - with just
    ///   one positional arg it's the pattern (`<find> [<path>…]`); if two or
    ///   more are given the second is an (ignored) replacement, so this
    ///   returns false and positional_skip falls back to 2.
    fn is_find_only(&self) -> bool {
        !self.uses_expressions() && (self.delete || (self.list_files && self.args.len() < 2))
    }

    fn diff_tool(&self) -> Option<String> {
        if let Some(ref tool) = self.diff_tool {
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

    fn file_set(&self) -> Option<fastmod::FileSet> {
        let globs = parse_file_globs(self.files.as_deref()?);
        if globs.is_empty() {
            return None;
        }
        Some(fastmod::FileSet {
            matches: globs,
            case_insensitive: true,
        })
    }

    fn walk(&self) -> Result<ignore::Walk> {
        Ok(
            fastmod::walk_builder_with_file_set(self.dirs(), self.file_set())?
                .hidden(!self.hidden)
                .build(),
        )
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Expression {
    find: String,
    replace: String,
}

struct CompiledExpression {
    pattern: String,
    regex: regex::Regex,
    matcher: grep::regex::RegexMatcher,
    replacer: Box<dyn Fn(&regex::Captures) -> String + Send + Sync>,
    /// Dispatch for `apply_compiled_expressions` - lets each mode use a
    /// `Replacer` impl that appends directly into the destination buffer
    /// instead of allocating a fresh `String` per match.
    bulk: BulkReplacer,
}

enum BulkReplacer {
    Literal(String),
    Regex(String),
    Smart(std::sync::Arc<std::collections::HashMap<String, String>>),
}

struct CountingLiteralReplacer<'a> {
    rep: &'a str,
    count: usize,
}

impl regex::Replacer for CountingLiteralReplacer<'_> {
    fn replace_append(&mut self, _: &regex::Captures<'_>, dst: &mut String) {
        self.count += 1;
        dst.push_str(self.rep);
    }
}

struct CountingRegexReplacer<'a> {
    subst: &'a str,
    count: usize,
}

impl regex::Replacer for CountingRegexReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::Captures<'_>, dst: &mut String) {
        self.count += 1;
        caps.expand(self.subst, dst);
    }
}

struct CountingSmartReplacer<'a> {
    map: &'a std::collections::HashMap<String, String>,
    count: usize,
}

impl regex::Replacer for CountingSmartReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::Captures<'_>, dst: &mut String) {
        self.count += 1;
        let matched = caps.get(0).unwrap().as_str();
        match self.map.get(matched) {
            Some(v) => dst.push_str(v),
            None => dst.push_str(matched),
        }
    }
}

fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

/// Parse the `-f` smart glob mini-DSL into iglob patterns for fastmod.
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

/// Build the 7 case variant pairs for preserve-case replacement.
/// Returns (variant_map, regex_pattern).
fn build_case_variants(
    pattern: &str,
    replacement: &str,
) -> (std::collections::HashMap<String, String>, String) {
    use inflector::cases::{
        camelcase::to_camel_case, kebabcase::to_kebab_case, pascalcase::to_pascal_case,
        screamingsnakecase::to_screaming_snake_case, snakecase::to_snake_case,
        traincase::to_train_case,
    };

    fn to_ada_case(input: &str) -> String {
        to_train_case(input).replace('-', "_")
    }

    let converters: &[fn(&str) -> String] = &[
        to_ada_case,
        to_camel_case,
        to_kebab_case,
        to_pascal_case,
        to_screaming_snake_case,
        to_snake_case,
        to_train_case,
    ];

    let mut map = std::collections::HashMap::new();
    let mut alt_parts = Vec::new();

    for convert in converters {
        let from = convert(pattern);
        let to = convert(replacement);
        if !from.is_empty() && !map.contains_key(&from) {
            alt_parts.push(regex::escape(&from));
            map.insert(from, to);
        }
    }

    // Sort longest first so regex alternation matches greedily
    alt_parts.sort_by_key(|a| std::cmp::Reverse(a.len()));
    let regex_pattern = alt_parts.join("|");

    (map, regex_pattern)
}

fn build_pattern_for(cli: &Cli, pattern: &str) -> String {
    let base = if !cli.is_regex() {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };

    let wrapped = if cli.line_regexp {
        format!("^(?:{base})$")
    } else if cli.word_regexp {
        format!(r"\b({base})\b")
    } else {
        base
    };

    let inner = if cli.is_regex() && !cli.greedy {
        format!("(?U){wrapped}")
    } else {
        wrapped
    };

    if cli.delete {
        wrap_delete_pattern(&inner, cli.line_regexp)
    } else {
        inner
    }
}

/// Extend a match pattern to consume the full line(s) it sits on, plus any
/// single trailing newline, so an empty replacement removes whole lines.
///
/// The user's pattern is kept inside a non-capturing group so an embedded
/// `(?U)` inverted-greediness flag stays scoped - otherwise the wrapper's
/// `[^\n]*` runs would flip to non-greedy and leave a tail of the line.
fn wrap_delete_pattern(inner: &str, line_regexp: bool) -> String {
    if line_regexp {
        // `inner` already anchors `^...$` for whole-line matches.
        format!(r"(?:{inner})\n?")
    } else {
        format!(r"^[^\n]*(?:{inner})[^\n]*\n?")
    }
}

fn build_subst_for(cli: &Cli, replacement: &str) -> String {
    if !cli.is_regex() {
        replacement.replace('$', "$$")
    } else {
        replacement.to_string()
    }
}

fn parse_expression(input: &str) -> Result<Expression> {
    let Some((find, replace)) = input.split_once('=') else {
        bail!("Invalid expression {input:?}: expected find=replace");
    };
    Ok(Expression {
        find: find.to_string(),
        replace: replace.to_string(),
    })
}

fn parse_expressions(cli: &Cli) -> Result<Vec<Expression>> {
    cli.expressions
        .iter()
        .map(|expr| parse_expression(expr))
        .collect()
}

fn compile_expression(cli: &Cli, expr: &Expression) -> Result<CompiledExpression> {
    if cli.smart {
        let (variant_map, pattern) = build_case_variants(&expr.find, &expr.replace);
        let regex = RegexBuilder::new(&pattern)
            .build()
            .with_context(|| format!("Invalid smart pattern: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new().build(&pattern)?;
        let variant_map = std::sync::Arc::new(variant_map);
        let closure_map = std::sync::Arc::clone(&variant_map);
        let replacer = move |caps: &regex::Captures| -> String {
            let matched = caps.get(0).unwrap().as_str();
            closure_map
                .get(matched)
                .cloned()
                .unwrap_or_else(|| matched.to_string())
        };
        Ok(CompiledExpression {
            pattern,
            regex,
            matcher,
            replacer: Box::new(replacer),
            bulk: BulkReplacer::Smart(variant_map),
        })
    } else {
        let pattern = build_pattern_for(cli, &expr.find);
        let subst = build_subst_for(cli, &expr.replace);
        let dot_matches_new_line = cli.dotall || cli.multiline;
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(dot_matches_new_line)
            .build()
            .with_context(|| format!("Invalid regex: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(dot_matches_new_line)
            .build(&pattern)?;
        let bulk = if cli.is_regex() {
            BulkReplacer::Regex(subst.clone())
        } else {
            BulkReplacer::Literal(expr.replace.clone())
        };
        let replacer = move |caps: &regex::Captures| -> String {
            let mut out = String::with_capacity(subst.len());
            caps.expand(&subst, &mut out);
            out
        };
        Ok(CompiledExpression {
            pattern,
            regex,
            matcher,
            replacer: Box::new(replacer),
            bulk,
        })
    }
}

fn build_pre_filter_matcher(
    cli: &Cli,
    expressions: &[CompiledExpression],
) -> Result<grep::regex::RegexMatcher> {
    if expressions.len() == 1 {
        return Ok(expressions[0].matcher.clone());
    }
    let union = expressions
        .iter()
        .map(|e| format!("(?:{})", e.pattern))
        .collect::<Vec<_>>()
        .join("|");
    let mut builder = RegexMatcherBuilder::new();
    if !cli.smart {
        builder
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(cli.dotall || cli.multiline);
    }
    builder
        .build(&union)
        .with_context(|| format!("Invalid union pre-filter pattern: {union}"))
}

fn compile_expressions(cli: &Cli) -> Result<Vec<CompiledExpression>> {
    let expressions = if cli.uses_expressions() {
        if cli.delete {
            // In delete mode, `-e` does not split on `=` — the whole argument
            // is the pattern to match for line deletion. This lets patterns
            // like `foo=bar` (containing a literal `=`) be matched verbatim.
            cli.expressions
                .iter()
                .map(|raw| Expression {
                    find: raw.clone(),
                    replace: String::new(),
                })
                .collect()
        } else {
            parse_expressions(cli)?
        }
    } else if cli.is_find_only() {
        vec![Expression {
            find: cli.pattern().to_string(),
            replace: String::new(),
        }]
    } else {
        vec![Expression {
            find: cli.pattern().to_string(),
            replace: cli.replacement().to_string(),
        }]
    };

    expressions
        .iter()
        .map(|expr| compile_expression(cli, expr))
        .collect()
}

fn apply_compiled_expressions<'a>(
    contents: &'a str,
    expressions: &[CompiledExpression],
) -> (Cow<'a, str>, usize) {
    use regex::Replacer as _;
    let mut current = Cow::Borrowed(contents);
    let mut replacements = 0;

    for expr in expressions {
        let (replaced, count) = match &expr.bulk {
            BulkReplacer::Literal(lit) => {
                let mut rep = CountingLiteralReplacer { rep: lit, count: 0 };
                let out = expr.regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Regex(subst) => {
                let mut rep = CountingRegexReplacer { subst, count: 0 };
                let out = expr.regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Smart(map) => {
                let mut rep = CountingSmartReplacer { map, count: 0 };
                let out = expr.regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
        };
        if count > 0 {
            replacements += count;
            current = Cow::Owned(replaced.into_owned());
        }
    }

    (current, replacements)
}

fn run_list_files(cli: &Cli) -> Result<()> {
    use std::sync::mpsc::channel;
    use std::thread;

    use ignore::WalkState;

    let expressions = compile_expressions(cli)?;
    let pre_filter = build_pre_filter_matcher(cli, &expressions)?;

    let walk = fastmod::walk_builder_with_file_set(cli.dirs(), cli.file_set())?
        .hidden(!cli.hidden)
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
        ))
        .build_parallel();

    let (tx, rx) = channel::<String>();

    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = fastmod::make_searcher();
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
                if !fastmod::looks_like_code(path) {
                    return WalkState::Continue;
                }
                if fastmod::file_matches(&mut searcher, &pre_filter, path)
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

    let walk = fastmod::walk_builder_with_file_set(cli.dirs(), cli.file_set())?
        .hidden(!cli.hidden)
        .threads(std::cmp::min(
            12,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
        ))
        .build_parallel();

    let (tx, rx) = channel::<Result<(String, usize)>>();
    let walk_expressions = Arc::clone(&expressions);

    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = fastmod::make_searcher();
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
                if !fastmod::looks_like_code(path) {
                    return WalkState::Continue;
                }
                let Some(contents) =
                    fastmod::file_contents_if_matches(&mut searcher, &pre_filter, path)
                else {
                    return WalkState::Continue;
                };
                let (updated, count) = apply_compiled_expressions(&contents, &expressions);
                if count == 0 {
                    return WalkState::Continue;
                }
                let payload = if write && let Err(e) = std::fs::write(path, updated.as_bytes()) {
                    Err(anyhow::Error::new(e).context(format!("Unable to write to {path:?}")))
                } else {
                    Ok((display_path(path), count))
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

    ok_results.sort_by(|a, b| natord::compare(&a.0, &b.0));
    print_summary(&ok_results, !write);
    Ok(())
}

fn run_preview(cli: &Cli) -> Result<()> {
    let expressions = compile_expressions(cli)?;
    let mut fm = fastmod::Fastmod::new(false, cli.hidden, cli.diff_tool());

    if expressions.len() == 1 {
        fm.present_and_apply_patches_with_replacer_walk(
            &expressions[0].regex,
            &expressions[0].matcher,
            &*expressions[0].replacer,
            cli.dirs(),
            cli.file_set(),
        )
    } else {
        let pre_filter = build_pre_filter_matcher(cli, &expressions)?;
        let mut searcher = fastmod::make_searcher();
        let expr_refs: Vec<fastmod::Replacer<'_>> = expressions
            .iter()
            .map(|e| {
                (
                    &e.regex,
                    &*e.replacer as &dyn Fn(&regex::Captures) -> String,
                )
            })
            .collect();

        for entry in cli.walk()? {
            let entry = entry?;
            if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                continue;
            }
            let path = entry.path();
            if !fastmod::looks_like_code(path) {
                continue;
            }
            let Some(contents) =
                fastmod::file_contents_if_matches(&mut searcher, &pre_filter, path)
            else {
                continue;
            };
            fm.present_and_apply_patches_multi(&expr_refs, path, contents)?;
        }
        Ok(())
    }
}

fn run_stdin(cli: &Cli) -> Result<()> {
    use std::io;
    let expressions = compile_expressions(cli)?;
    let input = io::read_to_string(io::stdin().lock())?;
    let (output, _) = apply_compiled_expressions(&input, &expressions);
    print!("{output}");
    Ok(())
}

/// Render `n` using the system locale's thousands separator (e.g. `648098` → `648,098`
/// on en_US, `648.098` on de_DE). Locales whose separator is whitespace (fr_FR's NBSP,
/// sv_SE's regular space, etc.) fall back to `,` because a space inside a count is
/// ambiguous in CLI output - it reads as a word boundary, not a digit group. Same
/// fallback when the system locale cannot be read at all.
fn with_commas(n: usize) -> String {
    use num_format::ToFormattedString as _;
    let fallback = || n.to_formatted_string(&num_format::Locale::en);
    let Ok(loc) = num_format::SystemLocale::default() else {
        return fallback();
    };
    if loc.separator().chars().all(char::is_whitespace) {
        return fallback();
    }
    n.to_formatted_string(&loc)
}

fn summary_message(total_files: usize, total_matches: usize, dry: bool) -> String {
    let verb = if dry { "Would perform" } else { "Performed" };
    format!(
        "{} {} replacement{} in {} file{}",
        verb,
        with_commas(total_matches),
        if total_matches == 1 { "" } else { "s" },
        with_commas(total_files),
        if total_files == 1 { "" } else { "s" },
    )
}

/// Print match summary. `dry` uses yellow "Would perform", otherwise green "Performed".
fn print_summary(results: &[(String, usize)], dry: bool) {
    let total_files = results.len();
    let total_matches: usize = results.iter().map(|(_, c)| c).sum();
    let stdout_tty = std::io::stdout().is_terminal();

    for (path, count) in results {
        let count = with_commas(*count);
        if stdout_tty {
            println!("{path} \x1b[38;5;248m({count})\x1b[m");
        } else {
            println!("{path} ({count})");
        }
    }

    if total_files > 0 {
        let msg = summary_message(total_files, total_matches, dry);
        if stdout_tty {
            let color = if dry { "\x1b[33m" } else { "\x1b[32m" };
            println!("\n\x1b[1m{color}{msg}\x1b[m");
        } else {
            println!("\n{msg}");
        }
    }
}

fn print_error(err: &anyhow::Error) {
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[1;31merror:\x1b[m {err}");
    } else {
        eprintln!("error: {err}");
    }
}

fn main() {
    if let Err(err) = run() {
        print_error(&err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "rep", &mut std::io::stdout());
        return Ok(());
    }

    if cli.help_long {
        print_help_long();
        std::process::exit(0);
    }

    if cli.help || (!cli.uses_expressions() && cli.args.is_empty()) {
        print_help();
        if cli.help {
            std::process::exit(0);
        } else {
            std::process::exit(1);
        }
    }

    if cli.positional_skip() > cli.args.len() {
        print_error(&anyhow::anyhow!("missing required argument: <replace>"));
        print_help();
        std::process::exit(1);
    }

    // Validate paths exist
    for dir in &cli.dirs() {
        if !std::path::Path::new(dir).exists() {
            bail!("{dir}: no such file or directory");
        }
    }

    if cli.list_files {
        return run_list_files(&cli);
    }

    let paths = cli.paths();
    let has_stdin_arg = paths.len() == 1 && paths[0].to_str() == Some("-");
    let is_piped = !std::io::stdin().is_terminal();
    let is_stdin_mode = has_stdin_arg || (is_piped && paths.is_empty());

    // Validation: smart + multiple paths
    if cli.smart && paths.len() > 1 {
        bail!("Smart mode only supports a single path");
    }

    // Validation: stdin + extra paths
    if is_stdin_mode && has_stdin_arg && paths.len() > 1 {
        bail!("Expected exactly 2 positional arguments when reading from stdin");
    }

    // Validation: preview requires interactive terminal
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

    fn build_pattern(cli: &Cli) -> String {
        build_pattern_for(cli, cli.pattern())
    }

    fn build_subst(cli: &Cli) -> String {
        build_subst_for(cli, cli.replacement())
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
    fn test_build_case_variants() {
        let (map, pattern) = build_case_variants("foo_bar", "spam_eggs");

        assert_eq!(map.get("foo_bar"), Some(&"spam_eggs".to_string()));
        assert_eq!(map.get("FooBar"), Some(&"SpamEggs".to_string()));
        assert_eq!(map.get("FOO_BAR"), Some(&"SPAM_EGGS".to_string()));
        assert_eq!(map.get("foo-bar"), Some(&"spam-eggs".to_string()));
        assert_eq!(map.get("fooBar"), Some(&"spamEggs".to_string()));
        assert_eq!(map.get("Foo-Bar"), Some(&"Spam-Eggs".to_string())); // train-case
        assert_eq!(map.get("Foo_Bar"), Some(&"Spam_Eggs".to_string())); // ada_case

        let regex = regex::Regex::new(&pattern).unwrap();
        assert!(regex.is_match("foo_bar"));
        assert!(regex.is_match("FooBar"));
        assert!(regex.is_match("FOO_BAR"));
        assert!(regex.is_match("foo-bar"));
        assert!(regex.is_match("fooBar"));
        assert!(!regex.is_match("foobar"));
    }

    #[test]
    fn test_build_case_variants_replacement() {
        let (map, pattern) = build_case_variants("foo_bar", "spam_eggs");
        let regex = regex::Regex::new(&pattern).unwrap();

        let input = "let foo_bar = FooBar::new(FOO_BAR);";
        let output = regex.replace_all(input, |caps: &regex::Captures| {
            map.get(caps.get(0).unwrap().as_str())
                .cloned()
                .unwrap_or_default()
        });
        assert_eq!(output, "let spam_eggs = SpamEggs::new(SPAM_EGGS);");
    }

    #[test]
    fn test_parse_expression_splits_on_first_equals() {
        assert_eq!(
            parse_expression("a=b=c").unwrap(),
            Expression {
                find: "a".to_string(),
                replace: "b=c".to_string(),
            }
        );
    }

    #[test]
    fn test_compile_expressions_applies_in_order() {
        let cli = Cli::parse_from(["rep", "-e", "a=b", "-e", "b=c", "src"]);

        assert_eq!(cli.paths(), vec![PathBuf::from("src")]);

        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("a b", &expressions);
        assert_eq!(output, "c c");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_expression_mode_without_paths_defaults_to_current_dir() {
        let cli = Cli::parse_from(["rep", "-e", "a=b", "-e", "b=c", "--dry-run"]);

        assert!(cli.paths().is_empty());
        assert_eq!(cli.dirs(), vec!["."]);
    }

    /// Regression: the preview-mode replacer was building new_contents as
    /// `contents[..offset] + repl + contents[offset+mat.end()..]`, dropping
    /// `contents[offset..offset+mat.start()]` - the text between the search
    /// window and the actual match position.
    #[test]
    fn test_expression_preserves_text_before_match() {
        let cli = Cli::parse_from(["rep", "-e", "a=b"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("#![allow(clippy::all)]", &expressions);
        assert_eq!(output, "#![bllow(clippy::bll)]");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_parse_expression_missing_equals() {
        assert!(parse_expression("no-equals-here").is_err());
    }

    #[test]
    fn test_apply_compiled_expressions_no_matches() {
        let cli = Cli::parse_from(["rep", "-e", "xyz=abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("hello world", &expressions);
        assert_eq!(output, "hello world");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_build_subst_escapes_dollar_in_literal_mode() {
        let cli = Cli::parse_from(["rep", "foo", "$1"]);
        assert_eq!(build_subst(&cli), "$$1");
    }

    #[test]
    fn test_build_subst_preserves_dollar_in_regex_mode() {
        let cli = Cli::parse_from(["rep", "-r", "(foo)", "$1"]);
        assert_eq!(build_subst(&cli), "$1");
    }

    #[test]
    fn test_build_pattern_escapes_metacharacters() {
        let cli = Cli::parse_from(["rep", "1.2.3", "4.5.6"]);
        assert_eq!(build_pattern(&cli), r"1\.2\.3");
    }

    #[test]
    fn test_build_pattern_regex_non_greedy_by_default() {
        let cli = Cli::parse_from(["rep", "-r", "a.*b", "x"]);
        assert_eq!(build_pattern(&cli), "(?U)a.*b");
    }

    #[test]
    fn test_build_pattern_regex_greedy() {
        let cli = Cli::parse_from(["rep", "-r", "-G", "a.*b", "x"]);
        assert_eq!(build_pattern(&cli), "a.*b");
    }

    #[test]
    fn test_build_pattern_word_boundary() {
        let cli = Cli::parse_from(["rep", "-w", "foo", "bar"]);
        assert_eq!(build_pattern(&cli), r"(?U)\b(foo)\b");
    }

    #[test]
    fn test_build_pattern_line_regexp() {
        let cli = Cli::parse_from(["rep", "-x", "foo", "bar"]);
        assert_eq!(build_pattern(&cli), "(?U)^(?:foo)$");
    }

    #[test]
    fn test_smart_replaces_case_variants() {
        let cli = Cli::parse_from(["rep", "foo_bar", "hello_world", "--smart"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_compiled_expressions("FooBar\nfoo_bar\nFOO_BAR\n", &expressions);
        assert_eq!(output, "HelloWorld\nhello_world\nHELLO_WORLD\n");
    }

    #[test]
    fn test_expression_with_line_regexp() {
        let cli = Cli::parse_from(["rep", "-x", "-e", "foo=bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foo\nfoobar\nfoo", &expressions);
        assert_eq!(output, "bar\nfoobar\nbar");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_expression_with_ignore_case() {
        let cli = Cli::parse_from(["rep", "-i", "-e", "foo=bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("Foo FOO foo", &expressions);
        assert_eq!(output, "bar bar bar");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_expression_with_word_boundary() {
        let cli = Cli::parse_from(["rep", "-w", "-e", "foo=bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foo foobar food", &expressions);
        assert_eq!(output, "bar foobar food");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_expression_with_regex_capture_groups() {
        let cli = Cli::parse_from(["rep", "-r", "-e", "(foo)\\.(bar)=$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foo.bar baz", &expressions);
        assert_eq!(output, "bar.foo baz");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_multiple_expressions_chain() {
        let cli = Cli::parse_from(["rep", "-e", "red=blue", "-e", "cat=dog"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_compiled_expressions("the red cat", &expressions);
        assert_eq!(output, "the blue dog");
    }

    #[test]
    fn test_expression_empty_replacement() {
        let cli = Cli::parse_from(["rep", "-e", "foo="]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foobarfoo", &expressions);
        assert_eq!(output, "bar");
        assert_eq!(count, 2);
    }

    /// Regression guard: in literal (non-regex) mode, `$1` in the replacement
    /// must be emitted verbatim. The `BulkReplacer::Literal` path uses
    /// `CountingLiteralReplacer` which does `push_str` without expansion -
    /// if it ever got routed through `caps.expand`, this test would fail.
    #[test]
    fn test_literal_mode_preserves_dollar_references() {
        let cli = Cli::parse_from(["rep", "foo", "$1bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foo baz", &expressions);
        assert_eq!(output, "$1bar baz");
        assert_eq!(count, 1);
    }

    /// Regression guard: with no matches, `apply_compiled_expressions` must
    /// return a `Cow::Borrowed` - no `String` allocation. Pins the zero-alloc
    /// contract so a future refactor can't silently force ownership.
    #[test]
    fn test_apply_compiled_expressions_no_matches_borrows() {
        let cli = Cli::parse_from(["rep", "-e", "xyz=abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_compiled_expressions("hello world", &expressions);
        assert!(matches!(output, Cow::Borrowed(_)));
    }

    /// `CountingSmartReplacer` has a `None => dst.push_str(matched)` fallback
    /// for matches not present in the variant map. In normal use the regex
    /// is built from the map keys so this branch is unreachable - test it
    /// directly to pin the contract.
    #[test]
    fn test_smart_replacer_fallback_to_matched() {
        use regex::Replacer as _;
        let map = std::collections::HashMap::new();
        let mut rep = CountingSmartReplacer {
            map: &map,
            count: 0,
        };
        let regex = regex::Regex::new("foo").unwrap();
        let output = regex.replace_all("foo bar foo", rep.by_ref());
        assert_eq!(output, "foo bar foo");
        assert_eq!(rep.count, 2);
    }

    #[test]
    fn test_dotall_allows_dot_to_match_newline() {
        let cli = Cli::parse_from(["rep", "-r", "--dotall", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("a\nb", &expressions);
        assert_eq!(output, "X");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_dot_does_not_match_newline_by_default() {
        let cli = Cli::parse_from(["rep", "-r", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("a\nb", &expressions);
        assert_eq!(output, "a\nb");
        assert_eq!(count, 0);
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
        assert_eq!(Cli::parse_from(["rep", "-e", "a=b"]).positional_skip(), 0);
        // -l with only a find: skip 1
        assert_eq!(Cli::parse_from(["rep", "-l", "a"]).positional_skip(), 1);
    }

    #[test]
    fn test_cli_is_find_only() {
        assert!(Cli::parse_from(["rep", "-l", "a"]).is_find_only());
        // -l with a replacement is NOT find-only (second arg is the ignored replacement)
        assert!(!Cli::parse_from(["rep", "-l", "a", "b"]).is_find_only());
        assert!(!Cli::parse_from(["rep", "a", "b"]).is_find_only());
        // -l with -e is expression mode, not find-only
        assert!(!Cli::parse_from(["rep", "-l", "-e", "a=b"]).is_find_only());
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
    fn test_delete_mode_wraps_pattern_and_deletes_whole_line() {
        let cli = Cli::parse_from(["rep", "-d", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions(
            "keep\nhas foo here\nkeep\nanother foo\ntail\n",
            &expressions,
        );
        assert_eq!(output, "keep\nkeep\ntail\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_handles_match_on_final_line_without_trailing_newline() {
        let cli = Cli::parse_from(["rep", "-d", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("keep\nhas foo", &expressions);
        assert_eq!(output, "keep\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_line_regexp_only_matches_exact_lines() {
        let cli = Cli::parse_from(["rep", "-d", "-x", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("foo\nfoobar\nfoo\nbar\n", &expressions);
        assert_eq!(output, "foobar\nbar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_ignore_case() {
        let cli = Cli::parse_from(["rep", "-d", "-i", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions("FOO line\nbar\nfoo line\n", &expressions);
        assert_eq!(output, "bar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_expression_takes_full_string_literally() {
        // `-d -e foo=bar` → the whole `foo=bar` is the pattern; no find/replace split.
        let cli = Cli::parse_from(["rep", "-d", "-e", "foo=bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) =
            apply_compiled_expressions("keep\nhas foo=bar here\nalso foo\ntail\n", &expressions);
        assert_eq!(output, "keep\nalso foo\ntail\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_multiple_expressions_deletes_each() {
        let cli = Cli::parse_from(["rep", "-d", "-e", "foo", "-e", "baz=qux"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_compiled_expressions(
            "keep\nhas foo\nmiddle\nline with baz=qux\ntail\n",
            &expressions,
        );
        assert_eq!(output, "keep\nmiddle\ntail\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_conflicts_with_smart_flag() {
        let result = Cli::try_parse_from(["rep", "-d", "-S", "foo"]);
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
    fn test_with_commas_formats_thousands() {
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(7), "7");
        assert_eq!(with_commas(999), "999");
        assert_eq!(with_commas(1_000), "1,000");
        assert_eq!(with_commas(12_345), "12,345");
        assert_eq!(with_commas(648_098), "648,098");
        assert_eq!(with_commas(1_000_000), "1,000,000");
    }

    #[test]
    fn test_summary_message_large_counts_use_thousands_separators() {
        assert_eq!(
            summary_message(718, 648_098, false),
            "Performed 648,098 replacements in 718 files"
        );
        assert_eq!(
            summary_message(1_000, 2_500_000, true),
            "Would perform 2,500,000 replacements in 1,000 files"
        );
    }

    /// `CompiledExpression::replacer` is the closure consumed by the preview
    /// path only - bulk apply goes through `BulkReplacer` and bypasses it.
    /// The three tests below invoke the closure directly (building `Captures`
    /// from the expression's own regex) so the closure bodies stay covered
    /// even though no unit test drives `run_preview`.
    #[test]
    fn test_preview_replacer_literal_mode_returns_raw_replacement() {
        let cli = Cli::parse_from(["rep", "foo", "$1bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let caps = expressions[0].regex.captures("foo").unwrap();
        assert_eq!((expressions[0].replacer)(&caps), "$1bar");
    }

    #[test]
    fn test_preview_replacer_regex_mode_expands_captures() {
        let cli = Cli::parse_from(["rep", "-r", r"(foo)\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let caps = expressions[0].regex.captures("foo.bar").unwrap();
        assert_eq!((expressions[0].replacer)(&caps), "bar.foo");
    }

    #[test]
    fn test_preview_replacer_smart_mode_maps_case_variant() {
        let cli = Cli::parse_from(["rep", "--smart", "foo_bar", "hello_world"]);
        let expressions = compile_expressions(&cli).unwrap();
        let caps = expressions[0].regex.captures("FooBar").unwrap();
        assert_eq!((expressions[0].replacer)(&caps), "HelloWorld");
    }
}

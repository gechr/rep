use crate::expressions::{EXPR_SEP, LineRange};
use crate::options::{MatchOptions, Mode, Options, WritePolicy};
use crate::ui::{Color, ColorChoice, Styles};
use crate::{config, scan};
use anyhow::{Result, bail};
use clap::builder::BoolishValueParser;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory as _, FromArgMatches as _, Parser};
use clap_complete::Shell;
use std::ffi::{OsStr, OsString};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

/// Parse defaults from an explicit snapshot, retaining provenance for conflicts.
pub(crate) fn parse_unresolved(
    argv: Vec<OsString>,
    env: &config::Environment,
) -> clap::error::Result<(Cli, ArgMatches, config::Origin)> {
    parse_with_config(argv, env, &config::Config::load(env))
}

fn parse_with_config(
    argv: Vec<OsString>,
    env: &config::Environment,
    config: &config::Config,
) -> clap::error::Result<(Cli, ArgMatches, config::Origin)> {
    let values = config.values();
    let mut origin = config::Origin::default();
    let mut command = Cli::command();
    let ids: Vec<_> = command
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect();
    for id in ids {
        let Some(name) = arg_env_name(&id) else {
            continue;
        };
        let value = if let Some(value) = env.get(name) {
            origin
                .sources
                .insert(id.clone(), config::Source::Environment);
            Some(value.to_os_string())
        } else if let Some(value) = values.get(name) {
            origin.sources.insert(id.clone(), config::Source::Config);
            Some(value.clone())
        } else {
            None
        };
        if let Some(value) = value {
            command = command.mut_arg(&id, |arg| arg.default_value(value));
        }
    }
    let matches = command.try_get_matches_from(preprocess_expression_args(argv))?;
    let mut cli = Cli::from_arg_matches(&matches)?;
    cli.no_color = env.no_color();
    Ok((cli, matches, origin))
}

#[derive(Parser)]
#[command(name = "rep", version, disable_help_flag = true)]
pub(crate) struct Cli {
    #[arg(skip)]
    pub(crate) no_color: bool,

    #[arg(value_name = "arg")]
    pub(crate) args: Vec<OsString>,

    #[arg(
        short = 'f',
        long = "files",
        value_name = "glob",
        help = "Smart glob patterns to match files against",
        help_heading = "Filter",
        overrides_with = "files"
    )]
    pub(crate) files: Option<String>,

    #[arg(
        short = 'H',
        long = "hidden",
        value_parser = BoolishValueParser::new(),
        help = "Search hidden files and directories",
        help_heading = "Filter"
    )]
    pub(crate) hidden: bool,

    #[arg(
        long = "no-ignore",
        value_parser = BoolishValueParser::new(),
        help = "Do not respect ignore files",
        help_heading = "Filter"
    )]
    pub(crate) no_ignore: bool,

    #[arg(
        short = 'e',
        long = "expression",
        value_name = "f> <r",
        allow_hyphen_values = true,
        help = "Repeatable <find> <replace> expression",
        help_heading = "Replace"
    )]
    pub(crate) expressions: Vec<String>,

    #[arg(
        short = 'S',
        long = "smart",
        value_parser = BoolishValueParser::new(),
        help = "Replace all <find> case variants",
        help_heading = "Replace"
    )]
    pub(crate) smart: bool,

    #[arg(
        short = 'P',
        long = "preserve",
        value_parser = BoolishValueParser::new(),
        help = "Mirror the <find> case onto the <replace>",
        help_heading = "Replace"
    )]
    pub(crate) preserve: bool,

    #[arg(
        short = 'G',
        long = "greedy",
        value_parser = BoolishValueParser::new(),
        help = "Use greedy matching for regular expressions",
        help_heading = "Match"
    )]
    pub(crate) greedy: bool,

    #[arg(
        short = 'i',
        long = "ignore-case",
        value_parser = BoolishValueParser::new(),
        help = "Case-insensitive matching",
        help_heading = "Match"
    )]
    pub(crate) ignore_case: bool,

    #[arg(
        short = 'm',
        long = "multiline",
        value_parser = BoolishValueParser::new(),
        help = "Search across multiple lines",
        help_heading = "Match"
    )]
    pub(crate) multiline: bool,

    #[arg(
        long = "dotall",
        value_parser = BoolishValueParser::new(),
        help = "Allow dot to match newlines",
        help_heading = "Match"
    )]
    pub(crate) dotall: bool,

    #[arg(
        short = 'r',
        long = "regex",
        alias = "regexp",
        value_parser = BoolishValueParser::new(),
        help = "Treat patterns as regular expressions",
        help_heading = "Match"
    )]
    pub(crate) regexp: bool,

    #[arg(
        short = 'w',
        long = "word-regexp",
        value_parser = BoolishValueParser::new(),
        help = "Match only whole words",
        help_heading = "Match"
    )]
    pub(crate) word_regexp: bool,

    #[arg(
        short = 'x',
        long = "line-regexp",
        value_parser = BoolishValueParser::new(),
        help = "Match only whole lines",
        help_heading = "Match"
    )]
    pub(crate) line_regexp: bool,

    #[arg(
        short = 'L',
        long = "line-range",
        alias = "lines",
        value_name = "range",
        value_parser = LineRange::parse,
        value_delimiter = ',',
        help = "Restrict matching to inclusive line ranges",
        help_heading = "Match"
    )]
    pub(crate) line_ranges: Vec<LineRange>,

    #[arg(
        short = 'd',
        long = "delete",
        help = "Delete lines matching <find>",
        help_heading = "Replace"
    )]
    pub(crate) delete: bool,

    #[arg(
        short = 'n',
        long = "dry-run",
        alias = "dry",
        value_parser = BoolishValueParser::new(),
        help = "Show what would be changed without writing",
        help_heading = "Mode"
    )]
    pub(crate) dry_run: bool,

    #[arg(
        short = 'W',
        short_alias = 'y',
        long = "write",
        alias = "yes",
        value_parser = BoolishValueParser::new(),
        help = "Apply changes to disk",
        help_heading = "Mode"
    )]
    pub(crate) write: bool,

    #[arg(
        short = 'p',
        long = "preview",
        value_parser = BoolishValueParser::new(),
        help = "Preview the changes before applying them",
        help_heading = "Mode"
    )]
    pub(crate) preview: bool,

    #[arg(
        long = "preview-tool",
        value_name = "cmd",
        overrides_with = "preview_tool",
        help = "External diff tool for preview mode",
        help_heading = "Mode"
    )]
    pub(crate) preview_tool: Option<String>,

    #[arg(
        short = 'l',
        long = "list-files",
        help = "Print file paths that would be changed",
        help_heading = "Mode"
    )]
    pub(crate) list_files: bool,

    #[arg(
        short = 'c',
        long = "count",
        help = "Print the total number of replacements",
        help_heading = "Mode"
    )]
    pub(crate) count: bool,

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
    pub(crate) context: usize,

    #[arg(
        long = "hyperlink-format",
        value_name = "fmt",
        overrides_with = "hyperlink_format",
        help = "Terminal hyperlink format",
        help_heading = "Miscellaneous",
        display_order = 100
    )]
    pub(crate) hyperlink_format: Option<String>,

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
    pub(crate) hyperlink_limit: u64,

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
    pub(crate) color: ColorChoice,

    #[arg(
        short = 'q',
        long = "quiet",
        value_parser = BoolishValueParser::new(),
        help = "Suppress diff output",
        help_heading = "Miscellaneous",
        display_order = 110
    )]
    pub(crate) quiet: bool,

    #[arg(
        long = "style-added",
        value_name = "style",
        help = "Style for added lines",
        help_heading = "Style",
        display_order = 10
    )]
    pub(crate) style_added: Option<String>,

    #[arg(
        long = "style-removed",
        value_name = "style",
        help = "Style for removed lines",
        help_heading = "Style",
        display_order = 20
    )]
    pub(crate) style_removed: Option<String>,

    #[arg(
        long = "style-line-added",
        value_name = "style",
        help = "Style for added line numbers",
        help_heading = "Style",
        display_order = 30
    )]
    pub(crate) style_line_added: Option<String>,

    #[arg(
        long = "style-line-removed",
        value_name = "style",
        help = "Style for removed line numbers",
        help_heading = "Style",
        display_order = 40
    )]
    pub(crate) style_line_removed: Option<String>,

    #[arg(
        long = "marker-added",
        value_name = "str",
        help = "Marker before added lines",
        help_heading = "Style",
        display_order = 60
    )]
    pub(crate) marker_added: Option<String>,

    #[arg(
        long = "marker-removed",
        value_name = "str",
        help = "Marker before removed lines",
        help_heading = "Style",
        display_order = 70
    )]
    pub(crate) marker_removed: Option<String>,

    #[arg(
        short = 'h',
        help = "Print short help",
        help_heading = "Miscellaneous",
        display_order = 130
    )]
    pub(crate) help: bool,

    #[arg(
        long = "help",
        help = "Print long help with examples",
        help_heading = "Miscellaneous",
        display_order = 140
    )]
    pub(crate) help_long: bool,

    #[arg(long = "completions", value_name = "shell", hide = true)]
    pub(crate) completions: Option<Shell>,

    #[arg(long = "no-hints", hide = true)]
    pub(crate) no_hints: bool,

    #[arg(
        long = "hints",
        value_parser = BoolishValueParser::new(),
        hide = true
    )]
    pub(crate) hints: bool,
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
        if v == "f> <r" {
            let blue = styles.fg(Color::Blue);
            let _ = write!(body, " {blue}{dim_attr}<{v}>{reset}");
        } else {
            let _ = write!(body, " {red}{dim_attr}<{v}>{reset}");
        }
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

/// Enforce the `config < env < CLI` precedence policy across mutually
/// exclusive flags. The winner of each group is the highest-priority "true"
/// flag (CLI > shell environment > config); the losers are cleared in
/// the resolved `Cli` so dispatch logic only sees one active flag per group.
/// Returns an error when two flags in the same group both come from the
/// same source tier (two CLI flags, two shell env vars, or two config
/// entries) - the genuine ambiguity cases.
pub(crate) fn resolve_mutex_groups(
    cli: &mut Cli,
    matches: &ArgMatches,
    origin: &config::Origin,
) -> Result<()> {
    let mut mode = resolve_group(
        matches,
        origin,
        &["dry_run", "write", "preview", "list_files"],
    )?;

    // Counting controls output and composes with the non-interactive write
    // policy (`--dry-run` or `--write`). Preview and file listing own their
    // output, so they remain mutually exclusive with count. Preserve the
    // normal source precedence when one of those modes came from env/config.
    if cli.count
        && let Some(mode_id @ ("preview" | "list_files")) = mode
    {
        let count_tier = tier_of("count", matches, origin).expect("active count has a source");
        let mode_tier = tier_of(mode_id, matches, origin).expect("active mode has a source");
        if count_tier == mode_tier {
            bail!(same_tier_error(count_tier, &[mode_id, "count"]));
        } else if count_tier > mode_tier {
            mode = None;
        } else {
            cli.count = false;
        }
    }

    cli.dry_run = mode == Some("dry_run");
    cli.write = mode == Some("write");
    cli.preview = mode == Some("preview");
    cli.list_files = mode == Some("list_files");

    if cli.list_files && preview_tool_active(cli, matches) {
        resolve_list_files_vs_preview_tool(cli, matches, origin)?;
    }

    // `--smart` and `--preserve` match `<find>` literally, so they exclude
    // `--regex` as well as each other.
    let case = resolve_group(matches, origin, &["smart", "preserve", "regexp"])?;
    cli.smart = case == Some("smart");
    cli.preserve = case == Some("preserve");
    cli.regexp = case == Some("regexp");

    let regex_anchor = resolve_group(matches, origin, &["word_regexp", "line_regexp"])?;
    cli.word_regexp = regex_anchor == Some("word_regexp");
    cli.line_regexp = regex_anchor == Some("line_regexp");

    cli.no_hints = !resolve_show_hints(matches, origin)?;
    cli.hints = !cli.no_hints;

    Ok(())
}

/// Resolve whether hint output should be shown. The default is "on" when
/// nothing is configured. CLI flags beat env; passing both `--hints` and
/// `--no-hints` on the command line is an error.
fn resolve_show_hints(matches: &ArgMatches, origin: &config::Origin) -> Result<bool> {
    let hints_cli = matches.value_source("hints") == Some(ValueSource::CommandLine);
    let no_hints_cli = matches.value_source("no_hints") == Some(ValueSource::CommandLine);
    if hints_cli && no_hints_cli {
        bail!("--hints and --no-hints cannot be used together");
    }
    if hints_cli {
        return Ok(matches.get_flag("hints"));
    }
    if no_hints_cli {
        return Ok(!matches.get_flag("no_hints"));
    }
    if let Some(Tier::ShellEnv | Tier::Config) = tier_of("hints", matches, origin) {
        return Ok(matches.get_flag("hints"));
    }
    Ok(true)
}

fn preview_tool_active(cli: &Cli, matches: &ArgMatches) -> bool {
    cli.preview_tool.is_some() && matches.contains_id("preview_tool")
}

fn resolve_list_files_vs_preview_tool(
    cli: &mut Cli,
    matches: &ArgMatches,
    origin: &config::Origin,
) -> Result<()> {
    let list_tier = tier_of("list_files", matches, origin);
    let tool_tier = tier_of("preview_tool", matches, origin);
    match (list_tier, tool_tier) {
        (Some(a), Some(b)) if a == b => bail!(same_tier_error(a, &["list_files", "preview_tool"])),
        (Some(a), Some(b)) if a > b => cli.preview_tool = None,
        (Some(_), Some(_)) => cli.list_files = false,
        _ => {}
    }
    Ok(())
}

/// Pick the winner of an "at most one is true" group. Higher tier wins;
/// same-tier conflicts are errors with wording specific to the source.
fn resolve_group<'a>(
    matches: &ArgMatches,
    origin: &config::Origin,
    ids: &[&'a str],
) -> Result<Option<&'a str>> {
    let mut by_tier: [Vec<&'a str>; Tier::COUNT] = std::array::from_fn(|_| Vec::new());
    for id in ids {
        if !matches.get_flag(id) {
            continue;
        }
        if let Some(tier) = tier_of(id, matches, origin) {
            by_tier[tier.index()].push(*id);
        }
    }
    // Walk tiers high-to-low: the highest-priority tier with any active
    // flag determines the winner. Same-tier ties become source-aware errors.
    for tier in [Tier::Cli, Tier::ShellEnv, Tier::Config] {
        let ids_in_tier = &by_tier[tier.index()];
        if ids_in_tier.len() > 1 {
            bail!(same_tier_error(tier, ids_in_tier));
        }
        if let Some(id) = ids_in_tier.first() {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Source tier for precedence resolution. Higher discriminant = higher
/// priority. The explicit `index` method (rather than `as usize` casts at
/// call sites) gives the compiler a chance to enforce exhaustiveness if a
/// variant is added.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Config,
    ShellEnv,
    Cli,
}

impl Tier {
    const COUNT: usize = 3;

    const fn index(self) -> usize {
        match self {
            Self::Config => 0,
            Self::ShellEnv => 1,
            Self::Cli => 2,
        }
    }
}

fn tier_of(id: &str, matches: &ArgMatches, origin: &config::Origin) -> Option<Tier> {
    match matches.value_source(id) {
        Some(ValueSource::CommandLine) => Some(Tier::Cli),
        Some(ValueSource::DefaultValue) => origin.sources.get(id).map(|source| match source {
            config::Source::Config => Tier::Config,
            config::Source::Environment => Tier::ShellEnv,
        }),
        _ => None,
    }
}

fn same_tier_error(tier: Tier, ids: &[&str]) -> String {
    let names: Vec<String> = ids.iter().map(|id| (*id).replace('_', "-")).collect();
    match tier {
        Tier::Cli => format!(
            "the following flags cannot be used together: {}",
            names
                .iter()
                .map(|n| format!("--{n}"))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
        Tier::ShellEnv => format!(
            "conflicting environment variables: {}",
            ids.iter()
                .map(|id| arg_env_name(id)
                    .map_or_else(|| format!("REP_{}", id.to_ascii_uppercase()), str::to_owned))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
        Tier::Config => format!(
            "config sets conflicting keys: {}",
            names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
    }
}

/// Map a configurable argument to its explicit environment source. Clap
/// itself never reads environment variables, keeping parsing deterministic.
pub(crate) fn arg_env_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "hidden" => "REP_HIDDEN",
        "no_ignore" => "REP_NO_IGNORE",
        "ignore_case" => "REP_IGNORE_CASE",
        "regexp" => "REP_REGEX",
        "multiline" => "REP_MULTILINE",
        "dotall" => "REP_DOTALL",
        "greedy" => "REP_GREEDY",
        "word_regexp" => "REP_WORD_REGEXP",
        "line_regexp" => "REP_LINE_REGEXP",
        "smart" => "REP_SMART",
        "preserve" => "REP_PRESERVE",
        "dry_run" => "REP_DRY_RUN",
        "write" => "REP_WRITE",
        "preview" => "REP_PREVIEW",
        "preview_tool" => "REP_PREVIEW_TOOL",
        "context" => "REP_CONTEXT",
        "color" => "REP_COLOR",
        "hyperlink_format" => "REP_HYPERLINK_FORMAT",
        "hyperlink_limit" => "REP_HYPERLINK_LIMIT",
        "quiet" => "REP_QUIET",
        "hints" => "REP_HINTS",
        "style_added" => "REP_STYLE_ADDED",
        "style_removed" => "REP_STYLE_REMOVED",
        "style_line_added" => "REP_STYLE_LINE_ADDED",
        "style_line_removed" => "REP_STYLE_LINE_REMOVED",
        "marker_added" => "REP_MARKER_ADDED",
        "marker_removed" => "REP_MARKER_REMOVED",
        _ => return None,
    })
}

/// The mode that would be active if the user passes no mode flag on the CLI.
/// Drives which mode flag gets promoted to the top of help output. Reads
/// the resolved `Cli`, so the help layout reflects the same precedence the
/// run path would apply.
const fn current_default_mode_id(cli: &Cli) -> &'static str {
    if cli.write {
        "write"
    } else if cli.preview {
        "preview"
    } else if cli.list_files {
        "list_files"
    } else {
        "dry_run"
    }
}

pub(crate) fn print_help(cli: &Cli, styles: Styles) {
    print_help_with(cli, HELP_SECTIONS, false, styles);
}

fn print_help_with(cli: &Cli, sections: &[&str], long: bool, styles: Styles) {
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
    let default_mode = current_default_mode_id(cli);

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

pub(crate) fn print_help_long(cli: &Cli, styles: Styles) {
    let bold = styles.bold();
    let green = styles.fg(Color::Green);
    let yellow = styles.fg(Color::Yellow);
    let grey = styles.fg(Color::Grey);
    let reset = styles.reset();

    print_help_with(cli, LONG_HELP_SECTIONS, true, styles);

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
    pub(crate) fn match_options(&self) -> Result<MatchOptions> {
        use crate::expressions::{Expression, parse_expression};

        let expressions = if self.uses_expressions() {
            self.expressions
                .iter()
                .map(|raw| {
                    if self.delete {
                        Ok(Expression {
                            find: raw
                                .split_once(EXPR_SEP)
                                .map_or(raw.as_str(), |(find, _)| find)
                                .to_owned(),
                            replace: String::new(),
                        })
                    } else {
                        parse_expression(raw)
                    }
                })
                .collect::<Result<Vec<_>>>()?
        } else if let Some(find) = self.args.first() {
            let find = find
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("<find> must be valid UTF-8"))?;
            let replace = if self.delete {
                ""
            } else {
                self.args
                    .get(1)
                    .map(|value| {
                        value
                            .to_str()
                            .ok_or_else(|| anyhow::anyhow!("<replace> must be valid UTF-8"))
                    })
                    .transpose()?
                    .unwrap_or_default()
            };
            vec![Expression {
                find: find.to_owned(),
                replace: replace.to_owned(),
            }]
        } else {
            Vec::new()
        };
        Ok(MatchOptions {
            expressions,
            smart: self.smart,
            preserve: self.preserve,
            greedy: self.greedy,
            ignore_case: self.ignore_case,
            multiline: self.multiline,
            dotall: self.dotall,
            regexp: self.regexp,
            word_regexp: self.word_regexp,
            line_regexp: self.line_regexp,
            line_ranges: self.line_ranges.clone(),
            delete: self.delete,
            preview: self.preview,
        })
    }

    pub(crate) fn into_options(self) -> Result<Options> {
        let matching = self.match_options()?;
        let paths = self.paths();
        let file_set = self.file_set();
        let has_pattern = self.uses_expressions() || !self.args.is_empty();
        let filter_by_change = !self.delete && (self.uses_expressions() || self.args.len() >= 2);
        let preview_tool = if self.preview {
            self.preview_tool()
        } else {
            None
        };
        Ok(Options {
            mode: if self.list_files {
                Mode::ListFiles
            } else if self.preview {
                Mode::Preview
            } else if self.count {
                Mode::Count
            } else {
                Mode::Replace
            },
            write_policy: if self.write {
                WritePolicy::Write
            } else {
                WritePolicy::DryRun
            },
            matching,
            paths,
            file_set,
            has_pattern,
            filter_by_change,
            hidden: self.hidden,
            no_ignore: self.no_ignore,
            preview_tool,
            context: self.context,
            hyperlink_format: self.hyperlink_format,
            hyperlink_limit: self.hyperlink_limit,
            color: self.color,
            no_color: self.no_color,
            quiet: self.quiet,
            no_hints: self.no_hints,
            style_added: self.style_added,
            style_removed: self.style_removed,
            style_line_added: self.style_line_added,
            style_line_removed: self.style_line_removed,
            marker_added: self.marker_added,
            marker_removed: self.marker_removed,
        })
    }

    pub(crate) const fn uses_expressions(&self) -> bool {
        !self.expressions.is_empty()
    }

    /// True when the CLI cannot take a `<replace>` positional.
    ///
    /// `-d`/`--delete` forbids `<replace>`; trailing positionals are paths.
    /// `-l` alone accepts an optional `<replace>`, so it is not find-only;
    /// see [`Self::positional_skip`].
    pub(crate) const fn is_find_only(&self) -> bool {
        !self.uses_expressions() && self.delete
    }

    pub(crate) fn preview_tool(&self) -> Option<String> {
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

    #[cfg(test)]
    pub(crate) const fn is_regex(&self) -> bool {
        self.regexp
            || self.dotall
            || self.multiline
            || self.ignore_case
            || self.greedy
            || self.word_regexp
            || self.line_regexp
    }

    pub(crate) fn positional_skip(&self) -> usize {
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

    #[cfg(test)]
    pub(crate) fn dirs(&self) -> Vec<&Path> {
        let args = &self.args[self.positional_skip().min(self.args.len())..];
        if args.is_empty() {
            vec![Path::new(".")]
        } else {
            args.iter().map(Path::new).collect()
        }
    }

    pub(crate) fn file_set(&self) -> Option<scan::FileSet> {
        let globs = parse_file_globs(self.files.as_deref()?);
        if globs.is_empty() {
            return None;
        }
        Some(scan::FileSet {
            matches: globs,
            case_insensitive: true,
        })
    }

    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.args
            .iter()
            .skip(self.positional_skip())
            .map(PathBuf::from)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn pattern(&self) -> &str {
        self.args[0]
            .to_str()
            .expect("find was validated during normalization")
    }

    #[cfg(test)]
    pub(crate) fn replacement(&self) -> &str {
        self.args[1]
            .to_str()
            .expect("replacement was validated during normalization")
    }
}

/// Preprocess argv so that `-e <find> <replace>` is compacted into a single
/// clap value joined by `EXPR_SEP` before clap parses the argument list.
/// This lets the second arg start with `-` without being treated as a flag.
///
/// Under `-d`/`--delete` there is no replace half, so `-e` consumes only a
/// single `<find>` token and any trailing positional is left as a path.
pub(crate) fn preprocess_expression_args<T: Into<OsString>>(args: Vec<T>) -> Vec<OsString> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let delete_mode = args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--delete" || a.to_str().is_some_and(short_cluster_has_delete));
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            out.push(arg);
            out.extend(iter);
            break;
        }
        if arg == "-e" || arg == "--expression" {
            out.push(arg);
            let Some(mut find) = iter.next() else {
                continue;
            };
            if !delete_mode && let Some(replace) = iter.next() {
                find.push(OsStr::new("\0"));
                find.push(replace);
            }
            out.push(find);
        } else if let Some(find) = arg
            .to_str()
            .and_then(|a| a.strip_prefix("-e"))
            .filter(|s| !s.is_empty())
        {
            let mut find = OsString::from(find);
            out.push(OsString::from("-e"));
            if !delete_mode && let Some(replace) = iter.next() {
                find.push(OsStr::new("\0"));
                find.push(replace);
            }
            out.push(find);
        } else {
            out.push(arg);
        }
    }
    out
}

/// Short flags that take no value and so can share one `-` cluster with
/// `-d`, as in `-nd` or `-Wd`. A value-taking short flag (`-e`, `-f`, `-L`,
/// `-C`) ends the cluster: the rest of the token is its value, so a `d` there
/// is data rather than `--delete`.
const BOOL_SHORT_FLAGS: &[char] = &[
    'H', 'S', 'P', 'G', 'i', 'm', 'r', 'w', 'x', 'n', 'W', 'y', 'p', 'l', 'c', 'q', 'h',
];

fn short_cluster_has_delete(arg: &str) -> bool {
    let Some(cluster) = arg.strip_prefix('-') else {
        return false;
    };
    if cluster.starts_with('-') {
        return false;
    }
    for c in cluster.chars() {
        if c == 'd' {
            return true;
        }
        if !BOOL_SHORT_FLAGS.contains(&c) {
            return false;
        }
    }
    false
}

pub(crate) fn parse_file_globs(input: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        try_parse_cli(args).unwrap()
    }
    fn try_parse_cli(args: &[&str]) -> clap::error::Result<Cli> {
        Cli::try_parse_from(preprocess_expression_args(args.to_vec()))
    }
    fn parse_and_resolve(args: &[&str]) -> Result<Cli> {
        resolve_sources(
            args,
            &config::Environment::default(),
            &config::Config::default(),
        )
    }
    fn resolve_sources(
        args: &[&str],
        env: &config::Environment,
        config: &config::Config,
    ) -> Result<Cli> {
        let argv = args.iter().map(OsString::from).collect();
        let (mut cli, matches, origin) = parse_with_config(argv, env, config)?;
        resolve_mutex_groups(&mut cli, &matches, &origin)?;
        Ok(cli)
    }

    #[test]
    fn explicit_source_precedence() {
        let config = config::Config {
            dry_run: Some(true),
            ..Default::default()
        };
        let env = config::Environment::from_pairs(&[("REP_WRITE", "true")]);
        let cli = resolve_sources(&["rep", "a", "b"], &env, &config).unwrap();
        assert!(cli.write);
        assert!(!cli.dry_run);
        let cli = resolve_sources(&["rep", "--dry-run", "a", "b"], &env, &config).unwrap();
        assert!(cli.dry_run);
        assert!(!cli.write);
    }

    #[test]
    fn explicit_false_masks_config() {
        let config = config::Config {
            write: Some(true),
            hidden: Some(true),
            hints: Some(true),
            ..Default::default()
        };
        let env = config::Environment::from_pairs(&[
            ("REP_WRITE", "false"),
            ("REP_HIDDEN", "0"),
            ("REP_HINTS", "no"),
        ]);
        let cli = resolve_sources(&["rep", "a", "b"], &env, &config).unwrap();
        assert!(!cli.write);
        assert!(!cli.hidden);
        assert!(cli.no_hints);
    }

    #[test]
    fn conflicts_retain_source_names() {
        let env =
            config::Environment::from_pairs(&[("REP_WRITE", "true"), ("REP_DRY_RUN", "true")]);
        let error = resolve_sources(&["rep", "a", "b"], &env, &config::Config::default())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "conflicting environment variables: REP_DRY_RUN / REP_WRITE"
        );
        let config = config::Config {
            write: Some(true),
            dry_run: Some(true),
            ..Default::default()
        };
        let error = resolve_sources(&["rep", "a", "b"], &config::Environment::default(), &config)
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "config sets conflicting keys: `dry-run` / `write`"
        );
    }

    #[test]
    fn cli_overrides_environment_groups() {
        for (key, flag) in [
            ("REP_PRESERVE", "--smart"),
            ("REP_LINE_REGEXP", "--word-regexp"),
            ("REP_HINTS", "--no-hints"),
        ] {
            let env = config::Environment::from_pairs(&[(key, "true")]);
            let cli = resolve_sources(&["rep", flag, "a", "b"], &env, &config::Config::default())
                .unwrap();
            match flag {
                "--smart" => assert!(cli.smart && !cli.preserve),
                "--word-regexp" => assert!(cli.word_regexp && !cli.line_regexp),
                _ => assert!(cli.no_hints && !cli.hints),
            }
        }
        let env = config::Environment::from_pairs(&[("REP_PREVIEW_TOOL", "delta")]);
        let cli = resolve_sources(
            &["rep", "--list-files", "a", "b"],
            &env,
            &config::Config::default(),
        )
        .unwrap();
        assert!(cli.list_files && cli.preview_tool.is_none());
    }

    #[test]
    fn malformed_defaults_fail_only_when_used() {
        let env = config::Environment::from_pairs(&[("REP_CONTEXT", "invalid")]);
        assert!(resolve_sources(&["rep", "a", "b"], &env, &config::Config::default()).is_err());
        assert_eq!(
            resolve_sources(
                &["rep", "--context", "4", "a", "b"],
                &env,
                &config::Config::default()
            )
            .unwrap()
            .context,
            4
        );
    }

    #[test]
    fn parsing_does_not_mutate_environment() {
        let before = std::env::var_os("REP_WRITE");
        let config = config::Config {
            write: Some(true),
            ..Default::default()
        };
        assert!(
            resolve_sources(&["rep", "a", "b"], &config::Environment::default(), &config)
                .unwrap()
                .write
        );
        assert_eq!(std::env::var_os("REP_WRITE"), before);
    }

    #[test]
    fn color_policy_survives_options_normalization() {
        let env = config::Environment::from_pairs(&[("NO_COLOR", "1")]);
        let cli = resolve_sources(&["rep", "a", "b"], &env, &config::Config::default()).unwrap();
        assert!(cli.into_options().unwrap().no_color);
        assert!(
            !parse_cli(&["rep", "a", "b"])
                .into_options()
                .unwrap()
                .no_color
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_survive_normalization() {
        use std::os::unix::ffi::OsStringExt as _;
        let path = OsString::from_vec(b"invalid-\xff.txt".to_vec());
        let cli = Cli::try_parse_from([
            OsString::from("rep"),
            "foo".into(),
            "bar".into(),
            path.clone(),
        ])
        .unwrap();
        let options = cli.into_options().unwrap();
        assert_eq!(options.paths, [PathBuf::from(path)]);
        assert_eq!(options.matching.expressions[0].find, "foo");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_patterns_are_rejected_without_panicking() {
        use std::os::unix::ffi::OsStringExt as _;
        let cli = Cli::try_parse_from([
            OsString::from("rep"),
            OsString::from_vec(b"\xff".to_vec()),
            "bar".into(),
        ])
        .unwrap();
        assert_eq!(
            cli.match_options().err().unwrap().to_string(),
            "<find> must be valid UTF-8"
        );
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
    fn test_mode_flags_are_mutex_on_cli() {
        for args in [
            ["rep", "--dry-run", "--write", "a", "b"].as_slice(),
            ["rep", "--write", "--preview", "a", "b"].as_slice(),
            ["rep", "--dry-run", "--preview", "a", "b"].as_slice(),
            ["rep", "--list-files", "--write", "a", "b"].as_slice(),
            ["rep", "--list-files", "--preview", "a", "b"].as_slice(),
            ["rep", "--list-files", "--dry-run", "a", "b"].as_slice(),
            ["rep", "--count", "--preview", "a", "b"].as_slice(),
            ["rep", "--count", "--list-files", "a", "b"].as_slice(),
        ] {
            assert!(
                parse_and_resolve(args).is_err(),
                "expected mode flags {args:?} to conflict, but parse succeeded"
            );
        }
    }

    #[test]
    fn test_count_composes_with_write_policy() {
        let write = parse_and_resolve(&["rep", "--write", "--count", "a", "b"]).unwrap();
        assert!(write.write);
        assert!(write.count);

        let dry_run = parse_and_resolve(&["rep", "--dry-run", "--count", "a", "b"]).unwrap();
        assert!(dry_run.dry_run);
        assert!(dry_run.count);
    }

    #[test]
    fn test_regex_is_mutex_with_smart_and_preserve_on_cli() {
        assert!(parse_and_resolve(&["rep", "--preserve", "--regex", "a", "b"]).is_err());
        assert!(parse_and_resolve(&["rep", "--smart", "-r", "a", "b"]).is_err());
        let cli = parse_and_resolve(&["rep", "-r", "a", "b"]).unwrap();
        assert!(cli.regexp, "regex alone stays on");
    }

    #[test]
    fn test_smart_and_preserve_are_mutex_on_cli() {
        assert!(parse_and_resolve(&["rep", "--smart", "--preserve", "a", "b"]).is_err());
    }

    #[test]
    fn test_word_and_line_regexp_are_mutex_on_cli() {
        assert!(parse_and_resolve(&["rep", "--word-regexp", "--line-regexp", "a", "b"]).is_err());
    }

    #[test]
    fn test_hints_and_no_hints_are_mutex_on_cli() {
        assert!(parse_and_resolve(&["rep", "--hints", "--no-hints", "a", "b"]).is_err());
    }

    #[test]
    fn test_expression_mode_without_paths_defaults_to_current_dir() {
        let cli = parse_cli(&["rep", "-e", "a", "b", "-e", "b", "c", "--dry-run"]);

        assert!(cli.paths().is_empty());
        assert_eq!(cli.dirs(), vec![Path::new(".")]);
    }

    #[test]
    fn test_expression_values_may_start_with_dash() {
        let cli = parse_cli(&[
            "rep",
            "--write",
            "-e",
            "- old value",
            "- new value",
            "notes.txt",
        ]);

        assert_eq!(
            cli.expressions,
            vec![format!("- old value{EXPR_SEP}- new value")]
        );
        assert_eq!(cli.paths(), vec![std::path::PathBuf::from("notes.txt")]);
    }

    #[test]
    fn test_cli_is_regex_any_flag_enables_regex() {
        assert!(!parse_cli(&["rep", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-r", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-i", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-w", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-x", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-m", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "-G", "a", "b"]).is_regex());
        assert!(parse_cli(&["rep", "--dotall", "a", "b"]).is_regex());
    }

    #[test]
    fn test_cli_collects_repeated_and_comma_separated_line_ranges() {
        let cli = parse_cli(&["rep", "-L", "2:3,3-4", "-L", "8", "foo", "bar"]);
        assert_eq!(
            cli.line_ranges,
            vec![
                LineRange {
                    start: 2,
                    end: Some(3),
                },
                LineRange {
                    start: 3,
                    end: Some(4),
                },
                LineRange {
                    start: 8,
                    end: Some(8),
                },
            ]
        );
    }

    #[test]
    fn test_cli_positional_skip() {
        // find+replace mode: skip 2 positional args
        assert_eq!(parse_cli(&["rep", "a", "b"]).positional_skip(), 2);
        // expression mode: no positional find/replace
        assert_eq!(parse_cli(&["rep", "-e", "a", "b"]).positional_skip(), 0);
        // -l accepts an optional <replace>: 1 positional stays find-only,
        // 2+ positionals consume both find and replace.
        assert_eq!(parse_cli(&["rep", "-l", "a"]).positional_skip(), 1);
        assert_eq!(parse_cli(&["rep", "-l", "a", "b"]).positional_skip(), 2);
        assert_eq!(
            parse_cli(&["rep", "-l", "a", "b", "src"]).positional_skip(),
            2
        );
        // -d -l keeps delete semantics (no <replace>).
        assert_eq!(
            parse_cli(&["rep", "-d", "-l", "a", "src"]).positional_skip(),
            1
        );
    }

    #[test]
    fn test_cli_is_find_only() {
        // -l is no longer find-only: it accepts an optional <replace>.
        assert!(!parse_cli(&["rep", "-l", "a"]).is_find_only());
        assert!(!parse_cli(&["rep", "-l", "a", "b"]).is_find_only());
        assert!(!parse_cli(&["rep", "a", "b"]).is_find_only());
        // -l with -e is expression mode, not find-only
        assert!(!parse_cli(&["rep", "-l", "-e", "a", "b"]).is_find_only());
        // -d is always find-only regardless of trailing positional path count
        assert!(parse_cli(&["rep", "-d", "a"]).is_find_only());
        assert!(parse_cli(&["rep", "-d", "a", "src"]).is_find_only());
        assert!(parse_cli(&["rep", "-d", "a", "src", "tests"]).is_find_only());
        // -d -l keeps delete's find-only semantics.
        assert!(parse_cli(&["rep", "-d", "-l", "a", "src"]).is_find_only());
    }

    #[test]
    fn test_delete_mode_treats_trailing_positionals_as_paths() {
        // With -d, there is no <replace>; args[1..] are all paths.
        let cli = parse_cli(&["rep", "-d", "TODO", "src", "tests"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_delete_in_short_cluster_keeps_expression_paths() {
        // `-nd` carries `-d`, so `-e` takes only `<find>` and the trailing
        // positional stays a path.
        let cli = parse_cli(&["rep", "-nd", "-e", "foo", "sub/a.txt"]);
        assert!(cli.delete, "cluster must enable delete");
        assert_eq!(cli.expressions, vec!["foo"]);
        assert_eq!(cli.paths(), vec![PathBuf::from("sub/a.txt")]);
    }

    #[test]
    fn test_short_cluster_has_delete_stops_at_value_flags() {
        assert!(short_cluster_has_delete("-d"));
        assert!(short_cluster_has_delete("-nd"));
        assert!(short_cluster_has_delete("-Wdn"));
        assert!(!short_cluster_has_delete("--delete"));
        assert!(!short_cluster_has_delete("-fd"));
        assert!(!short_cluster_has_delete("-ed"));
        assert!(!short_cluster_has_delete("-nfd"));
        assert!(!short_cluster_has_delete("d"));
        assert!(!short_cluster_has_delete("-"));
    }

    #[test]
    fn test_positionals_after_double_dash_are_not_rewritten() {
        let cli = parse_cli(&["rep", "-n", "--", "-enable", "enable", "b.txt"]);
        assert!(cli.expressions.is_empty(), "no `-e` given");
        assert_eq!(cli.pattern(), "-enable");
        assert_eq!(cli.replacement(), "enable");
        assert_eq!(cli.paths(), vec![PathBuf::from("b.txt")]);

        let cli = parse_cli(&["rep", "-n", "--", "-e", "x", "b.txt"]);
        assert!(cli.expressions.is_empty(), "no `-e` given");
        assert_eq!(cli.pattern(), "-e");
        assert_eq!(cli.replacement(), "x");
    }

    #[test]
    fn test_list_files_mode_consumes_optional_replace() {
        // 1 positional: find-only, default search root.
        let cli = parse_cli(&["rep", "-l", "TODO"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(cli.paths(), Vec::<PathBuf>::new());

        // 2 positionals: <find> <replace>, default search root.
        let cli = parse_cli(&["rep", "-l", "foo", "bar"]);
        assert_eq!(cli.positional_skip(), 2);
        assert_eq!(cli.pattern(), "foo");
        assert_eq!(cli.replacement(), "bar");
        assert_eq!(cli.paths(), Vec::<PathBuf>::new());

        // 3+ positionals: <find> <replace> followed by paths.
        let cli = parse_cli(&["rep", "-l", "foo", "bar", "src", "tests"]);
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
        let cli = parse_cli(&["rep", "-d", "-l", "TODO", "src", "tests"]);
        assert_eq!(cli.positional_skip(), 1);
        assert_eq!(cli.pattern(), "TODO");
        assert_eq!(
            cli.paths(),
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn test_delete_combines_with_smart_flag() {
        let cli = parse_cli(&["rep", "-d", "-S", "foo_bar"]);
        assert!(cli.delete);
        assert!(cli.smart);
    }

    #[test]
    fn test_delete_combines_with_list_files() {
        let cli = parse_cli(&["rep", "-d", "-l", "foo"]);
        assert!(cli.delete);
        assert!(cli.list_files);
    }

    #[test]
    fn test_color_flag_parses_all_variants() {
        assert_eq!(
            parse_cli(&["rep", "--color=auto", "a", "b"]).color,
            ColorChoice::Auto
        );
        assert_eq!(
            parse_cli(&["rep", "--color=always", "a", "b"]).color,
            ColorChoice::Always
        );
        assert_eq!(
            parse_cli(&["rep", "--color=never", "a", "b"]).color,
            ColorChoice::Never
        );
        // Default when omitted.
        assert_eq!(parse_cli(&["rep", "a", "b"]).color, ColorChoice::Auto);
    }

    #[test]
    fn test_color_flag_rejects_invalid_value() {
        assert!(try_parse_cli(&["rep", "--color=bogus", "a", "b"]).is_err());
    }

    #[test]
    fn test_cli_dirs_defaults_to_current_directory() {
        assert_eq!(parse_cli(&["rep", "a", "b"]).dirs(), vec![Path::new(".")]);
    }

    #[test]
    fn test_cli_dirs_uses_trailing_positionals() {
        let cli = parse_cli(&["rep", "a", "b", "src", "tests"]);
        assert_eq!(cli.dirs(), vec![Path::new("src"), Path::new("tests")]);
    }

    #[test]
    fn test_cli_paths_skips_find_and_replace() {
        let cli = parse_cli(&["rep", "a", "b", "src", "tests"]);
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
}

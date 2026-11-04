//! Mode-specific runners over the shared scanner, transformation, and writer.

use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::engine::{self, Candidate};
use crate::expressions::{
    CompiledExpression, apply_compiled_expressions, build_pre_filter_matcher, compile_expressions,
    expressions_match_in_ranges, first_column_map_for_expressions, first_column_map_if_needed,
    line_range_windows, output_first_column_map,
};
use crate::hyperlink::{
    display_path, hyperlink_format, hyperlink_format_uses_column, hyperlink_path,
};
use crate::interactive::{self, PreviewOutcome};
use crate::options::{Mode, Options, WritePolicy};
use crate::output::{self, ReplacementResult, ResultPrinter};
use crate::scan;
use crate::theme;
use crate::ui::{Color, ColorChoice, Styles};

#[cfg(unix)]
fn stdin_has_input() -> bool {
    use std::fs::File;
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::fs::FileTypeExt as _;
    let fd = io::stdin().as_raw_fd();
    // SAFETY: ManuallyDrop leaves the borrowed stdin descriptor open.
    let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    file.metadata()
        .is_ok_and(|m| m.is_file() || m.file_type().is_fifo())
}

#[cfg(not(unix))]
fn stdin_has_input() -> bool {
    !io::stdin().is_terminal()
}

pub(crate) fn print_error(err: &anyhow::Error) {
    let styles = Styles::resolve(ColorChoice::Auto, io::stderr().is_terminal(), no_color());
    eprintln!(
        "{}{}error:{} {err:#}",
        styles.bold(),
        styles.fg(Color::Red),
        styles.reset()
    );
}

pub(crate) fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

pub(crate) fn run(options: &Options) -> Result<i32> {
    let theme = theme::Theme::from_overrides(theme::Overrides {
        style_added: options.style_added.as_deref(),
        style_removed: options.style_removed.as_deref(),
        style_line_added: options.style_line_added.as_deref(),
        style_line_removed: options.style_line_removed.as_deref(),
        marker_added: options.marker_added.clone(),
        marker_removed: options.marker_removed.clone(),
    })
    .map_err(|error| anyhow::anyhow!("invalid style: {error}"))?;
    let styles = Styles::resolve(options.color, io::stdout().is_terminal(), options.no_color)
        .with_theme(&theme);
    let stdin_arg =
        options.mode != Mode::ListFiles && options.paths.iter().any(|p| p == Path::new("-"));
    if stdin_arg && options.paths.len() > 1 {
        bail!("Cannot mix `-` (stdin) with other paths");
    }
    if options.mode != Mode::ListFiles && options.matching.smart && options.paths.len() > 1 {
        bail!("Smart mode only supports a single path");
    }
    if options.mode == Mode::Preview && !io::stdin().is_terminal() {
        bail!("--preview requires an interactive terminal");
    }
    let stdin = stdin_arg || (options.paths.is_empty() && stdin_has_input());
    if stdin && options.mode != Mode::ListFiles {
        return run_stdin(options).map(|()| 0);
    }
    let dirs = if options.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        options.paths.clone()
    };
    let (paths, errors) = scan::discover(
        &dirs,
        options.file_set.clone(),
        options.hidden,
        options.no_ignore,
    )?;
    match options.mode {
        Mode::ListFiles => run_list(options, &paths, errors).map(|()| 0),
        Mode::Preview => run_preview(options, &paths, errors, styles),
        Mode::Replace | Mode::Count => run_files(options, &paths, errors, styles).map(|()| 0),
    }
}

fn finish_errors(errors: &[anyhow::Error], writing: bool, output: io::Result<()>) -> Result<()> {
    for error in errors {
        print_error(error);
    }
    let output = match output {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    };
    if !errors.is_empty() {
        if let Err(error) = output {
            print_error(&error.into());
        }
        let n = errors.len();
        let noun = if n == 1 { "file" } else { "files" };
        if writing
            && errors
                .iter()
                .all(|e| e.to_string().starts_with("Unable to write to "))
        {
            bail!("{n} {noun} could not be written");
        }
        let noun = if n == 1 { "path" } else { "paths" };
        bail!("{n} {noun} could not be processed");
    }
    output?;
    Ok(())
}

fn run_stdin(options: &Options) -> Result<()> {
    let expressions = compile_expressions(&options.matching)?;
    let mut input = Vec::new();
    io::stdin().lock().read_to_end(&mut input)?;
    let result =
        apply_compiled_expressions(&input, &expressions, false, &options.matching.line_ranges);
    let mut out = io::BufWriter::with_capacity(output::BUFFER_CAPACITY, io::stdout().lock());
    let written = if options.mode == Mode::Count {
        writeln!(
            out,
            "{}",
            if result.output.as_ref() == input {
                0
            } else {
                result.count
            }
        )
    } else {
        out.write_all(&result.output)
    };
    finish_errors(&[], false, written.and_then(|()| out.flush()))
}

fn run_list(options: &Options, paths: &[Candidate], mut errors: Vec<anyhow::Error>) -> Result<()> {
    let matching = &options.matching;
    let expressions = compile_expressions(matching)?;
    let no_pattern = !options.has_pattern;
    let by_change = options.filter_by_change;
    let prefilter = (!no_pattern)
        .then(|| build_pre_filter_matcher(matching, &expressions))
        .transpose()?;
    let line_oriented = !by_change
        && matching.line_ranges.is_empty()
        && expressions.iter().all(|e| e.preserves_line_boundaries);
    let mut out = io::BufWriter::with_capacity(output::BUFFER_CAPACITY, io::stdout().lock());
    let mut output_error = None;
    engine::ordered(
        paths,
        || {
            (
                if line_oriented {
                    scan::make_line_searcher()
                } else {
                    scan::make_searcher()
                },
                Vec::new(),
            )
        },
        |(searcher, scratch), path| -> Result<Option<PathBuf>> {
            let listed = match &prefilter {
                None => true,
                Some(filter) if by_change || !matching.line_ranges.is_empty() => {
                    let Some(contents) =
                        scan::file_contents_if_matches(searcher, filter, path, scratch)?
                    else {
                        return Ok(None);
                    };
                    if by_change {
                        let result = apply_compiled_expressions(
                            &contents,
                            &expressions,
                            false,
                            &matching.line_ranges,
                        );
                        result.count > 0 && result.output.as_ref() != contents
                    } else {
                        expressions_match_in_ranges(&contents, &expressions, &matching.line_ranges)
                    }
                }
                Some(filter) => scan::file_matches(searcher, filter, path)?,
            };
            Ok(listed.then(|| path.to_path_buf()))
        },
        |result| {
            match result {
                Ok(Some(path)) => {
                    if let Err(error) = writeln!(out, "{}", display_path(&path)) {
                        output_error = Some(error);
                        return false;
                    }
                }
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
            true
        },
    );
    finish_errors(
        &errors,
        false,
        output_error.map_or_else(|| out.flush(), Err),
    )
}

struct Prepared {
    count: usize,
    rendered: Option<output::Rendered>,
    warning: Option<String>,
}

fn run_files(
    options: &Options,
    paths: &[Candidate],
    mut errors: Vec<anyhow::Error>,
    styles: Styles<'_>,
) -> Result<()> {
    let matching = &options.matching;
    let expressions = compile_expressions(matching)?;
    let writing = options.write_policy == WritePolicy::Write;
    let count_only = options.mode == Mode::Count;
    let colored = io::stdout().is_terminal() || options.color == ColorChoice::Always;
    let silent = !count_only && options.quiet && (writing || !colored);
    let format = options
        .hyperlink_format
        .as_deref()
        .and_then(hyperlink_format);
    let printer = ResultPrinter {
        styles,
        quiet: options.quiet,
        delete: matching.delete,
        dry: !writing,
        no_hints: options.no_hints,
        hyperlink_format: format.as_deref(),
        hyperlink_limit: options.hyperlink_limit,
        context_lines: options.context,
    };
    let out = io::BufWriter::with_capacity(output::BUFFER_CAPACITY, io::stdout().lock());
    let mut sink = output::Sink::new(&printer, out, colored && !silent && !count_only);
    let filter = build_pre_filter_matcher(matching, &expressions)?;
    let single = expressions.len() == 1;
    let first_column = !count_only && format.as_deref().is_some_and(hyperlink_format_uses_column);
    let inline = !count_only && colored && !options.quiet && single;
    let track_spans = inline || first_column;
    let linewise = colored && expressions.iter().all(|e| e.preserves_line_boundaries);
    let multiline_spans = inline
        && !matching.is_regex()
        && !matching.delete
        && expressions.iter().any(|e| !e.preserves_line_boundaries);
    let mut total = 0usize;
    engine::ordered(
        paths,
        || (scan::make_searcher(), Vec::new()),
        |(searcher, scratch), path| -> Result<Option<Prepared>> {
            if expressions.is_empty() {
                return Ok(None);
            }
            let mut owned = None;
            let contents: &[u8] = if single {
                if !scan::read_text_file(path, scratch)? {
                    return Ok(None);
                }
                scratch
            } else {
                owned = scan::file_contents_if_matches(searcher, &filter, path, scratch)?;
                let Some(contents) = &owned else {
                    return Ok(None);
                };
                contents
            };
            let result = apply_compiled_expressions(
                contents,
                &expressions,
                track_spans,
                &matching.line_ranges,
            );
            if result.count == 0 || result.output.as_ref() == contents {
                return Ok(None);
            }
            if writing {
                crate::write::commit(path, contents, &result.output)?;
            }
            let count = result.count;
            if count_only || silent {
                return Ok(Some(Prepared {
                    count,
                    rendered: None,
                    warning: None,
                }));
            }
            let columns = if single {
                first_column_map_if_needed(first_column, contents, &result.spans)
            } else {
                first_column_map_for_expressions(
                    first_column,
                    contents,
                    &expressions,
                    &matching.line_ranges,
                )
            };
            let out_columns = output_first_column_map(
                first_column && multiline_spans && writing,
                &result.output,
                &result.spans,
            );
            let updated = result.output.into_owned();
            let spans = result.spans;
            let old = owned.unwrap_or_else(|| std::mem::take(scratch));
            let mut warning = None;
            let diff = if options.quiet {
                None
            } else {
                match (String::from_utf8(old), String::from_utf8(updated)) {
                    (Ok(old), Ok(new)) => Some((old, new)),
                    _ => {
                        warning = Some(if writing {
                            format!(
                                "Warning: {}: rewritten without showing a diff (not valid UTF-8)",
                                path.display()
                            )
                        } else {
                            format!(
                                "Warning: {}: skipping diff (not valid UTF-8; use non-dry-run mode)",
                                path.display()
                            )
                        });
                        None
                    }
                }
            };
            let result = ReplacementResult {
                path: display_path(path),
                link_path: if format.is_some() {
                    hyperlink_path(path)
                } else {
                    String::new()
                },
                count,
                diff,
                columns,
                out_columns,
                spans: if inline { spans } else { Vec::new() },
                linewise_diff: linewise,
                multiline_span_diff: multiline_spans,
            };
            Ok(Some(Prepared {
                count,
                rendered: Some(printer.render(&result, colored)?),
                warning,
            }))
        },
        |result| {
            match result {
                Ok(Some(result)) => {
                    total = total.saturating_add(result.count);
                    if let Some(warning) = result.warning {
                        eprintln!("{warning}");
                    }
                    if let Some(rendered) = result.rendered
                        && !sink.push(&rendered)
                    {
                        return writing;
                    }
                }
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
            true
        },
    );
    let finished = sink.finish();
    let finished = if count_only {
        finished.and_then(|()| {
            let mut out = io::stdout().lock();
            writeln!(out, "{total}")?;
            out.flush()
        })
    } else {
        finished
    };
    finish_errors(&errors, writing, finished)
}

fn run_preview(
    options: &Options,
    paths: &[Candidate],
    mut errors: Vec<anyhow::Error>,
    styles: Styles<'_>,
) -> Result<i32> {
    let expressions = compile_expressions(&options.matching)?;
    let filter = build_pre_filter_matcher(&options.matching, &expressions)?;
    let refs: Vec<_> = expressions
        .iter()
        .map(CompiledExpression::preview_expr)
        .collect();
    let mut patcher = interactive::InteractivePatcher::new(
        false,
        options.preview_tool.clone(),
        options.context,
        styles,
    );
    let mut searcher = scan::make_searcher();
    let mut scratch = Vec::new();
    let mut exit = 0;
    for candidate in paths {
        let path = &candidate.path;
        if expressions.is_empty() {
            break;
        }
        let contents =
            match scan::file_contents_if_matches(&mut searcher, &filter, path, &mut scratch) {
                Ok(Some(contents)) => contents,
                Ok(None) => continue,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
        let Ok(contents) = String::from_utf8(contents) else {
            eprintln!(
                "Warning: {}: skipping (not valid UTF-8; use non-preview mode)",
                path.display()
            );
            continue;
        };
        let windows = line_range_windows(contents.as_bytes(), &options.matching.line_ranges);
        if windows.is_empty() {
            continue;
        }
        match patcher.present_and_apply_patches_multi(&refs, path, contents, windows) {
            Ok(PreviewOutcome::Continue) => {}
            Ok(PreviewOutcome::Quit) => break,
            Ok(PreviewOutcome::Interrupted) => {
                exit = 130;
                break;
            }
            Err(error) => errors.push(error.context(format!("Unable to preview {path:?}"))),
        }
    }
    finish_errors(&errors, true, Ok(()))?;
    Ok(exit)
}

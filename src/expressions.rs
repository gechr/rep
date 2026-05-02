// Expression compilation: `-e find=replace` / positional / `-d` /
// `--smart` modes, shared pre-filter matcher, and the counting bulk
// replacers used by the non-interactive apply path.

use std::borrow::Cow;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use grep::regex::RegexMatcher;
use grep::regex::RegexMatcherBuilder;
use regex::RegexBuilder;
use regex::bytes::RegexBuilder as BytesRegexBuilder;

use crate::Cli;

/// Internal separator used by `preprocess_expression_args` to join the two
/// space-separated `-e <find> <replace>` args into a single clap value.
/// Null byte is safe because Unix argv strings are null-terminated C strings
/// and can never contain one.
pub(crate) const EXPR_SEP: char = '\x00';

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Expression {
    pub(crate) find: String,
    pub(crate) replace: String,
}

pub(crate) struct CompiledExpression {
    pub(crate) pattern: String,
    pub(crate) regex: regex::Regex,
    pub(crate) bytes_regex: regex::bytes::Regex,
    pub(crate) matcher: RegexMatcher,
    pub(crate) replacer: Box<dyn Fn(&regex::Captures) -> String + Send + Sync>,
    /// Dispatch for `apply_compiled_expressions` - lets each mode use a
    /// `Replacer` impl that appends directly into the destination buffer
    /// instead of allocating a fresh `Vec<u8>` per match.
    pub(crate) bulk: BulkReplacer,
}

impl CompiledExpression {
    pub(crate) fn preview_expr(&self) -> crate::interactive::PreviewExpr<'_> {
        crate::interactive::PreviewExpr {
            regex: &self.regex,
            replacer: &*self.replacer,
        }
    }
}

pub(crate) enum BulkReplacer {
    Literal(String),
    Regex(String),
    Smart(std::sync::Arc<std::collections::HashMap<String, String>>),
}

struct CountingLiteralReplacer<'a> {
    rep: &'a [u8],
    count: usize,
}

impl regex::bytes::Replacer for CountingLiteralReplacer<'_> {
    fn replace_append(&mut self, _: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        dst.extend_from_slice(self.rep);
    }
}

struct CountingRegexReplacer<'a> {
    subst: &'a [u8],
    count: usize,
}

impl regex::bytes::Replacer for CountingRegexReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        caps.expand(self.subst, dst);
    }
}

struct CountingSmartReplacer<'a> {
    map: &'a std::collections::HashMap<String, String>,
    count: usize,
}

impl regex::bytes::Replacer for CountingSmartReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        let matched = caps.get(0).expect("full regex match is always present");
        // Smart-replace patterns are built from inflector case conversions of
        // the user's find string. Those are always valid UTF-8, so every
        // match here is a UTF-8 substring of the haystack - the `from_utf8`
        // always succeeds. A non-UTF-8 substring could never have matched.
        let key = std::str::from_utf8(matched.as_bytes())
            .expect("smart pattern alternatives are always UTF-8");
        dst.extend_from_slice(
            self.map
                .get(key)
                .expect("smart replacer map must contain every regex alternative")
                .as_bytes(),
        );
    }
}

/// Build the 7 case variant pairs for preserve-case replacement.
/// Returns (variant_map, regex_pattern).
pub(crate) fn build_case_variants(
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

    fn normalize_separators(input: &str) -> String {
        input.replace(['_', '-'], " ")
    }

    let mut map = std::collections::HashMap::new();
    let mut alt_parts = Vec::new();
    let seeds = [
        (pattern.to_string(), replacement.to_string()),
        (
            normalize_separators(pattern),
            normalize_separators(replacement),
        ),
    ];

    for (pattern_seed, replacement_seed) in seeds {
        for convert in converters {
            let from = convert(&pattern_seed);
            let to = convert(&replacement_seed);
            if !from.is_empty() && !map.contains_key(&from) {
                alt_parts.push(regex::escape(&from));
                map.insert(from, to);
            }
        }
    }

    // Sort longest first so regex alternation matches greedily
    alt_parts.sort_by_key(|a| std::cmp::Reverse(a.len()));
    let regex_pattern = alt_parts.join("|");

    (map, regex_pattern)
}

pub(crate) fn build_pattern_for(cli: &Cli, pattern: &str) -> String {
    let base = if !cli.is_regex() {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };

    let wrapped = if cli.line_regexp {
        format!("^(?:{base})$")
    } else if cli.word_regexp {
        format!(r"\b(?:{base})\b")
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

pub(crate) fn build_subst_for(cli: &Cli, replacement: &str) -> String {
    if !cli.is_regex() {
        replacement.replace('$', "$$")
    } else {
        replacement.to_string()
    }
}

fn parse_expression(input: &str) -> Result<Expression> {
    let Some((find, replace)) = input.split_once(EXPR_SEP) else {
        bail!("Invalid expression: expected `-e <find> <replace>`");
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
        let (variant_map, variant_pattern) = build_case_variants(&expr.find, &expr.replace);
        let wrapped = if cli.line_regexp {
            format!("^(?:{variant_pattern})$")
        } else if cli.word_regexp {
            format!(r"\b(?:{variant_pattern})\b")
        } else {
            variant_pattern
        };
        // With `-d --smart`, the case-variant alternation becomes the "inner" of
        // a line-deleting wrapper, and the replacement is always empty.
        let pattern = if cli.delete {
            wrap_delete_pattern(&wrapped, cli.line_regexp)
        } else {
            wrapped
        };
        let regex = RegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid smart pattern: {}", expr.find))?;
        let bytes_regex = BytesRegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid smart pattern: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new()
            .multi_line(true)
            .build(&pattern)?;
        if cli.delete {
            return Ok(CompiledExpression {
                pattern,
                regex,
                bytes_regex,
                matcher,
                replacer: Box::new(|_: &regex::Captures| String::new()),
                bulk: BulkReplacer::Literal(String::new()),
            });
        }
        let variant_map = std::sync::Arc::new(variant_map);
        let closure_map = std::sync::Arc::clone(&variant_map);
        let replacer = move |caps: &regex::Captures| -> String {
            let matched = caps
                .get(0)
                .expect("full regex match is always present")
                .as_str();
            closure_map
                .get(matched)
                .cloned()
                .expect("smart replacer map must contain every regex alternative")
        };
        Ok(CompiledExpression {
            pattern,
            regex,
            bytes_regex,
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
        let bytes_regex = BytesRegexBuilder::new(&pattern)
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
            bytes_regex,
            matcher,
            replacer: Box::new(replacer),
            bulk,
        })
    }
}

pub(crate) fn build_pre_filter_matcher(
    cli: &Cli,
    expressions: &[CompiledExpression],
) -> Result<RegexMatcher> {
    if expressions.len() == 1 {
        return Ok(expressions[0].matcher.clone());
    }
    let union = expressions
        .iter()
        .map(|e| format!("(?:{})", e.pattern))
        .collect::<Vec<_>>()
        .join("|");
    let mut builder = RegexMatcherBuilder::new();
    builder
        .multi_line(true)
        .dot_matches_new_line(cli.dotall || cli.multiline);
    if !cli.smart {
        builder.case_insensitive(cli.ignore_case);
    }
    builder
        .build(&union)
        .with_context(|| format!("Invalid union pre-filter pattern: {union}"))
}

pub(crate) fn compile_expressions(cli: &Cli) -> Result<Vec<CompiledExpression>> {
    let expressions = if cli.uses_expressions() {
        if cli.delete {
            // In delete mode the replace half of `-e <find> <replace>` is
            // ignored. Extract only the find part (before the \x00 separator).
            cli.expressions
                .iter()
                .map(|raw| Expression {
                    find: raw
                        .split_once(EXPR_SEP)
                        .map_or(raw.as_str(), |(f, _)| f)
                        .to_string(),
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
        .filter(|expr| cli.is_regex() || cli.smart || expr.find != expr.replace)
        .map(|expr| compile_expression(cli, expr))
        .collect()
}

pub(crate) fn apply_compiled_expressions<'a>(
    contents: &'a [u8],
    expressions: &[CompiledExpression],
) -> (Cow<'a, [u8]>, usize) {
    use regex::bytes::Replacer as _;
    let mut current: Cow<'a, [u8]> = Cow::Borrowed(contents);
    let mut replacements = 0;

    for expr in expressions {
        let (replaced, count) = match &expr.bulk {
            BulkReplacer::Literal(rep) => {
                let mut rep = CountingLiteralReplacer {
                    rep: rep.as_bytes(),
                    count: 0,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Regex(subst) => {
                let mut rep = CountingRegexReplacer {
                    subst: subst.as_bytes(),
                    count: 0,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Smart(map) => {
                let mut rep = CountingSmartReplacer { map, count: 0 };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let processed =
            crate::preprocess_expression_args(args.iter().map(|s| s.to_string()).collect());
        Cli::parse_from(processed)
    }

    fn build_pattern(cli: &Cli) -> String {
        build_pattern_for(cli, cli.pattern())
    }

    fn build_subst(cli: &Cli) -> String {
        build_subst_for(cli, cli.replacement())
    }

    fn apply_str<'a>(
        contents: &'a str,
        expressions: &[CompiledExpression],
    ) -> (Cow<'a, str>, usize) {
        let (out, n) = apply_compiled_expressions(contents.as_bytes(), expressions);
        let cow = match out {
            Cow::Borrowed(b) => Cow::Borrowed(std::str::from_utf8(b).unwrap()),
            Cow::Owned(o) => Cow::Owned(String::from_utf8(o).unwrap()),
        };
        (cow, n)
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
            map.get(
                caps.get(0)
                    .expect("full regex match is always present")
                    .as_str(),
            )
            .cloned()
            .expect("smart replacer map must contain every regex alternative")
        });
        assert_eq!(output, "let spam_eggs = SpamEggs::new(SPAM_EGGS);");
    }

    #[test]
    fn test_parse_expression_splits_on_null_byte() {
        assert_eq!(
            parse_expression(&format!("a{EXPR_SEP}b=c")).unwrap(),
            Expression {
                find: "a".to_string(),
                replace: "b=c".to_string(),
            }
        );
    }

    #[test]
    fn test_compile_expressions_applies_in_order() {
        let cli = parse_cli(&["rep", "-e", "a", "b", "-e", "b", "c", "src"]);

        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a b", &expressions);
        assert_eq!(output, "c c");
        assert_eq!(count, 3);
    }

    /// Regression: the preview-mode replacer was building new_contents as
    /// `contents[..offset] + repl + contents[offset+mat.end()..]`, dropping
    /// `contents[offset..offset+mat.start()]` - the text between the search
    /// window and the actual match position.
    #[test]
    fn test_expression_preserves_text_before_match() {
        let cli = parse_cli(&["rep", "-e", "a", "b"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("#![allow(clippy::all)]", &expressions);
        assert_eq!(output, "#![bllow(clippy::bll)]");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_parse_expression_missing_null_byte() {
        assert!(parse_expression("no-null-here").is_err());
    }

    #[test]
    fn test_apply_compiled_expressions_no_matches() {
        let cli = parse_cli(&["rep", "-e", "xyz", "abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("hello world", &expressions);
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
        assert_eq!(build_pattern(&cli), r"(?U)\b(?:foo)\b");
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
        let (output, _) = apply_str("FooBar\nfoo_bar\nFOO_BAR\n", &expressions);
        assert_eq!(output, "HelloWorld\nhello_world\nHELLO_WORLD\n");
    }

    #[test]
    fn test_smart_word_regexp_anchors_case_variants_at_word_boundaries() {
        let cli = Cli::parse_from(["rep", "-S", "-w", "Repo", "Repository"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Reports Repo repo REPO", &expressions);
        assert_eq!(output, "Reports Repository repository REPOSITORY");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_smart_line_regexp_anchors_case_variants_to_whole_lines() {
        let cli = Cli::parse_from(["rep", "-S", "-x", "Repo", "Repository"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Repo\nReports\nfoo Repo bar\n", &expressions);
        assert_eq!(output, "Repository\nReports\nfoo Repo bar\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_pre_filter_matcher_anchors_at_line_boundaries_for_smart_multi_expr() {
        use grep::matcher::Matcher as _;
        let cli = parse_cli(&[
            "rep",
            "-S",
            "-x",
            "-e",
            "Repo",
            "Repository",
            "-e",
            "Api",
            "Apis",
        ]);
        let expressions = compile_expressions(&cli).unwrap();
        let matcher = build_pre_filter_matcher(&cli, &expressions).unwrap();
        assert!(matcher.is_match(b"head\nRepo\ntail\n").unwrap());
        assert!(matcher.is_match(b"foo\napi\nbar\n").unwrap());
    }

    #[test]
    fn test_delete_smart_matches_kebab_variant_inside_larger_token() {
        let cli = Cli::parse_from(["rep", "-d", "foo_bar", "--smart"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("keep\nprefix-foo-bar-suffix\nkeep\n", &expressions);
        assert_eq!(output, "keep\nkeep\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_expression_with_line_regexp() {
        let cli = parse_cli(&["rep", "-x", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo\nfoobar\nfoo", &expressions);
        assert_eq!(output, "bar\nfoobar\nbar");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_expression_with_ignore_case() {
        let cli = parse_cli(&["rep", "-i", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Foo FOO foo", &expressions);
        assert_eq!(output, "bar bar bar");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_expression_with_word_boundary() {
        let cli = parse_cli(&["rep", "-w", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo foobar food", &expressions);
        assert_eq!(output, "bar foobar food");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_word_regexp_preserves_regex_capture_numbers() {
        let cli = parse_cli(&["rep", "-r", "-w", "-e", r"(foo)\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo.bar foo.baz", &expressions);
        assert_eq!(output, "bar.foo foo.baz");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_expression_with_regex_capture_groups() {
        let cli = parse_cli(&["rep", "-r", "-e", "(foo)\\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo.bar baz", &expressions);
        assert_eq!(output, "bar.foo baz");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_multiple_expressions_chain() {
        let cli = parse_cli(&["rep", "-e", "red", "blue", "-e", "cat", "dog"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("the red cat", &expressions);
        assert_eq!(output, "the blue dog");
    }

    #[test]
    fn test_expression_empty_replacement() {
        let cli = parse_cli(&["rep", "-e", "foo", ""]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foobarfoo", &expressions);
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
        let (output, count) = apply_str("foo baz", &expressions);
        assert_eq!(output, "$1bar baz");
        assert_eq!(count, 1);
    }

    /// Regression guard: with no matches, `apply_compiled_expressions` must
    /// return a `Cow::Borrowed` - no `String` allocation. Pins the zero-alloc
    /// contract so a future refactor can't silently force ownership.
    #[test]
    fn test_noop_expression_find_eq_replace_is_skipped() {
        let cli = parse_cli(&["rep", "-e", "foo", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        assert!(expressions.is_empty());
        let (output, count) = apply_str("foo bar foo", &expressions);
        assert_eq!(output, "foo bar foo");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_apply_compiled_expressions_no_matches_borrows() {
        let cli = parse_cli(&["rep", "-e", "xyz", "abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("hello world", &expressions);
        assert!(matches!(output, Cow::Borrowed(_)));
    }

    #[test]
    fn test_dotall_allows_dot_to_match_newline() {
        let cli = Cli::parse_from(["rep", "-r", "--dotall", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a\nb", &expressions);
        assert_eq!(output, "X");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_dot_does_not_match_newline_by_default() {
        let cli = Cli::parse_from(["rep", "-r", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a\nb", &expressions);
        assert_eq!(output, "a\nb");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_delete_mode_wraps_pattern_and_deletes_whole_line() {
        let cli = Cli::parse_from(["rep", "-d", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str(
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
        let (output, count) = apply_str("keep\nhas foo", &expressions);
        assert_eq!(output, "keep\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_line_regexp_only_matches_exact_lines() {
        let cli = Cli::parse_from(["rep", "-d", "-x", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo\nfoobar\nfoo\nbar\n", &expressions);
        assert_eq!(output, "foobar\nbar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_ignore_case() {
        let cli = Cli::parse_from(["rep", "-d", "-i", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("FOO line\nbar\nfoo line\n", &expressions);
        assert_eq!(output, "bar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_expression_takes_full_string_literally() {
        // `-d -e "foo=bar" ""` → find is `foo=bar` (the first arg); replace is ignored.
        let cli = parse_cli(&["rep", "-d", "-e", "foo=bar", ""]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("keep\nhas foo=bar here\nalso foo\ntail\n", &expressions);
        assert_eq!(output, "keep\nalso foo\ntail\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_multiple_expressions_deletes_each() {
        let cli = parse_cli(&["rep", "-d", "-e", "foo", "", "-e", "baz=qux", ""]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str(
            "keep\nhas foo\nmiddle\nline with baz=qux\ntail\n",
            &expressions,
        );
        assert_eq!(output, "keep\nmiddle\ntail\n");
        assert_eq!(count, 2);
    }

    /// `CompiledExpression::preview_expr()` hands the TUI a `PreviewExpr`
    /// wrapping the same regex + replacer closure used during preview. Bulk
    /// apply goes through `BulkReplacer` and bypasses this closure, so these
    /// tests exercise the preview-only code path directly.
    #[test]
    fn test_preview_replacer_literal_mode_returns_raw_replacement() {
        let cli = Cli::parse_from(["rep", "foo", "$1bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("foo").unwrap();
        assert_eq!((preview.replacer)(&caps), "$1bar");
    }

    #[test]
    fn test_preview_replacer_regex_mode_expands_captures() {
        let cli = Cli::parse_from(["rep", "-r", r"(foo)\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("foo.bar").unwrap();
        assert_eq!((preview.replacer)(&caps), "bar.foo");
    }

    #[test]
    fn test_preview_replacer_smart_mode_maps_case_variant() {
        let cli = Cli::parse_from(["rep", "--smart", "foo_bar", "hello_world"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("FooBar").unwrap();
        assert_eq!((preview.replacer)(&caps), "HelloWorld");
    }
}

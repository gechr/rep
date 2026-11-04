//! Resolved execution settings, independent of argument parsing.

use std::path::PathBuf;

use crate::expressions::{Expression, LineRange};
use crate::scan::FileSet;
use crate::ui::ColorChoice;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Replace,
    Preview,
    ListFiles,
    Count,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WritePolicy {
    DryRun,
    Write,
}

#[derive(Clone, Default)]
pub(crate) struct MatchOptions {
    pub(crate) expressions: Vec<Expression>,
    pub(crate) smart: bool,
    pub(crate) preserve: bool,
    pub(crate) greedy: bool,
    pub(crate) ignore_case: bool,
    pub(crate) multiline: bool,
    pub(crate) dotall: bool,
    pub(crate) regexp: bool,
    pub(crate) word_regexp: bool,
    pub(crate) line_regexp: bool,
    pub(crate) line_ranges: Vec<LineRange>,
    pub(crate) delete: bool,
    pub(crate) preview: bool,
}

impl MatchOptions {
    pub(crate) const fn is_regex(&self) -> bool {
        self.regexp
            || self.dotall
            || self.multiline
            || self.ignore_case
            || self.greedy
            || self.word_regexp
            || self.line_regexp
    }
}

pub(crate) struct Options {
    pub(crate) mode: Mode,
    pub(crate) write_policy: WritePolicy,
    pub(crate) matching: MatchOptions,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) file_set: Option<FileSet>,
    pub(crate) has_pattern: bool,
    pub(crate) filter_by_change: bool,
    pub(crate) hidden: bool,
    pub(crate) no_ignore: bool,
    pub(crate) preview_tool: Option<String>,
    pub(crate) context: usize,
    pub(crate) hyperlink_format: Option<String>,
    pub(crate) hyperlink_limit: u64,
    pub(crate) color: ColorChoice,
    pub(crate) no_color: bool,
    pub(crate) quiet: bool,
    pub(crate) no_hints: bool,
    pub(crate) style_added: Option<String>,
    pub(crate) style_removed: Option<String>,
    pub(crate) style_line_added: Option<String>,
    pub(crate) style_line_removed: Option<String>,
    pub(crate) marker_added: Option<String>,
    pub(crate) marker_removed: Option<String>,
}

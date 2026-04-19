// Walking + grep pre-filter + candidate-path check.
//
// Derived from fastmod (Copyright Meta Platforms, Inc. and affiliates),
// used under the Apache License, Version 2.0. See LICENSE and NOTICE
// at the repo root for details.

use std::cmp::min;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::channel;
use std::thread;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::ensure;
use grep::regex::RegexMatcher;
use grep::searcher::BinaryDetection;
use grep::searcher::Searcher;
use grep::searcher::SearcherBuilder;
use grep::searcher::Sink;
use grep::searcher::SinkMatch;
use ignore::WalkBuilder;
use ignore::WalkState;
use ignore::overrides::OverrideBuilder;

type Result<T> = ::std::result::Result<T, Error>;

#[derive(Clone)]
pub(crate) struct FileSet {
    pub(crate) matches: Vec<String>,
    pub(crate) case_insensitive: bool,
}

pub(crate) fn walk_builder_with_file_set(
    dirs: Vec<&str>,
    file_set: Option<FileSet>,
) -> Result<WalkBuilder> {
    ensure!(!dirs.is_empty(), "must provide at least one path to walk!");
    let mut builder = WalkBuilder::new(dirs[0]);
    for dir in &dirs[1..] {
        builder.add(dir);
    }
    if let Some(file_set) = file_set {
        let mut override_builder = OverrideBuilder::new(".");
        if file_set.case_insensitive {
            override_builder
                .case_insensitive(true)
                .context("Unable to toggle case sensitivity")?;
        }
        for file in file_set.matches {
            override_builder
                .add(&file)
                .context("Unable to register glob with directory walker")?;
        }
        builder.overrides(
            override_builder
                .build()
                .context("Unable to register glob with directory walker")?,
        );
    }
    Ok(builder)
}

pub(crate) fn apply_walk_flags(builder: &mut WalkBuilder, hidden: bool, no_ignore: bool) {
    builder.hidden(!hidden);
    if no_ignore {
        builder
            .ignore(false)
            .git_ignore(false)
            .git_exclude(false)
            .git_global(false);
    }
}

pub(crate) fn make_searcher() -> Searcher {
    SearcherBuilder::new()
        .line_number(false)
        .multi_line(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .bom_sniffing(false)
        .build()
}

pub(crate) fn file_contents_if_matches(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
) -> Option<Vec<u8>> {
    let mut sink = MatchSink::new();
    if let Err(e) = searcher.search_path(matcher, path, &mut sink) {
        eprintln!("Warning: {}: {e}", path.display());
    }
    if sink.did_match {
        match fs::read(path) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("Warning: {}: {e}", path.display());
                None
            }
        }
    } else {
        None
    }
}

pub(crate) fn file_matches(searcher: &mut Searcher, matcher: &RegexMatcher, path: &Path) -> bool {
    let mut sink = MatchSink::new();
    if let Err(e) = searcher.search_path(matcher, path, &mut sink) {
        eprintln!("Warning: {}: {e}", path.display());
    }
    sink.did_match
}

pub(crate) fn is_candidate_path(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    !bytes.ends_with(b"~") && !bytes.ends_with(b"tags") && !bytes.ends_with(b"TAGS")
}

/// Walk `dirs` in parallel, keep files that pass `is_candidate_path` and
/// match `pre_filter`, and stream `(path, contents)` pairs back on the
/// current thread. The walk runs on a background thread; the returned
/// iterator yields results as they arrive and terminates when the walk
/// finishes.
pub(crate) fn matching_files_parallel(
    dirs: Vec<&str>,
    file_set: Option<FileSet>,
    hidden: bool,
    no_ignore: bool,
    pre_filter: &RegexMatcher,
) -> Result<mpsc::IntoIter<(PathBuf, Vec<u8>)>> {
    let mut builder = walk_builder_with_file_set(dirs, file_set)?;
    apply_walk_flags(&mut builder, hidden, no_ignore);
    let walk = builder
        .threads(min(
            12,
            thread::available_parallelism().map_or(1, |n| n.get()),
        ))
        .build_parallel();
    let (tx, rx) = channel();
    let thread_matcher = pre_filter.clone();
    thread::spawn(move || {
        walk.run(|| {
            let mut searcher = make_searcher();
            let tx = tx.clone();
            let matcher = thread_matcher.clone();
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
                if !is_candidate_path(path) {
                    return WalkState::Continue;
                }
                if let Some(contents) = file_contents_if_matches(&mut searcher, &matcher, path)
                    && tx.send((path.to_path_buf(), contents)).is_err()
                {
                    return WalkState::Quit;
                }
                WalkState::Continue
            })
        });
    });
    Ok(rx.into_iter())
}

struct MatchSink {
    did_match: bool,
}

impl MatchSink {
    fn new() -> Self {
        Self { did_match: false }
    }
}

impl Sink for MatchSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        _mat: &SinkMatch,
    ) -> std::result::Result<bool, std::io::Error> {
        self.did_match = true;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::is_candidate_path;

    #[test]
    fn test_is_candidate_path_accepts_regular_source_file() {
        assert!(is_candidate_path(Path::new("src/main.rs")));
        assert!(is_candidate_path(Path::new("README.md")));
        assert!(is_candidate_path(Path::new("Makefile")));
    }

    #[test]
    fn test_is_candidate_path_rejects_tilde_backup() {
        assert!(!is_candidate_path(Path::new("main.rs~")));
        assert!(!is_candidate_path(Path::new("some/dir/file.txt~")));
    }

    #[test]
    fn test_is_candidate_path_rejects_ctags_files() {
        assert!(!is_candidate_path(Path::new("tags")));
        assert!(!is_candidate_path(Path::new("TAGS")));
        assert!(!is_candidate_path(Path::new("./tags")));
    }
}

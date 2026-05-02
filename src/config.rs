//! `~/.reprc` loader.
//!
//! Returns a sequence of CLI arguments parsed from a flat rc file (one flag
//! per line, `#` for comments, blank lines ignored). The arguments are
//! prepended to argv before clap parses, so a CLI flag naturally overrides
//! the same flag in the rc file.

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read};
use std::path::{Path, PathBuf};

const ENV_VAR: &str = "REP_CONFIG_PATH";
const DEFAULT_FILE: &str = ".reprc";

/// Resolve the rc path, then load and parse it. Returns an empty vec if no
/// rc exists. Read or parse errors are printed to stderr but never abort.
pub fn rc_args() -> Vec<OsString> {
    let Some(path) = rc_path() else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }
    match parse(&path) {
        Ok((args, errs)) => {
            for err in errs {
                eprintln!("rep: {}: {err}", path.display());
            }
            args
        }
        Err(err) => {
            eprintln!("rep: failed to read {}: {err}", path.display());
            Vec::new()
        }
    }
}

fn rc_path() -> Option<PathBuf> {
    // An explicitly-set `REP_CONFIG_PATH` always wins, even if it points
    // somewhere that doesn't exist - that's how the user disables the
    // default `~/.reprc` lookup. An empty value means "no rc".
    if let Some(value) = std::env::var_os(ENV_VAR) {
        if value.is_empty() {
            return None;
        }
        return Some(PathBuf::from(value));
    }
    #[allow(deprecated)]
    let home = std::env::home_dir()?;
    Some(home.join(DEFAULT_FILE))
}

fn parse(path: &Path) -> std::io::Result<(Vec<OsString>, Vec<String>)> {
    let file = std::fs::File::open(path)?;
    Ok(parse_reader(file))
}

fn parse_reader<R: Read>(rdr: R) -> (Vec<OsString>, Vec<String>) {
    let mut args = Vec::new();
    let mut errs = Vec::new();
    for (idx, line) in BufReader::new(rdr).lines().enumerate() {
        let line_no = idx + 1;
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                errs.push(format!("{line_no}: {err}"));
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        args.push(OsString::from(trimmed));
    }
    (args, errs)
}

#[cfg(test)]
mod tests {
    use super::parse_reader;
    use std::ffi::OsString;

    fn parse(input: &str) -> Vec<OsString> {
        let (args, errs) = parse_reader(input.as_bytes());
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        args
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let args = parse(
            "\
# leading comment
--hidden

   --ignore-case
   # indented comment
--preview-tool=delta
",
        );
        assert_eq!(
            args,
            vec![
                OsString::from("--hidden"),
                OsString::from("--ignore-case"),
                OsString::from("--preview-tool=delta"),
            ]
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let args = parse("   --hidden   \n\t--regex\t\n");
        assert_eq!(
            args,
            vec![OsString::from("--hidden"), OsString::from("--regex")]
        );
    }

    #[test]
    fn empty_file_yields_no_args() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n# only comments\n").is_empty());
    }
}

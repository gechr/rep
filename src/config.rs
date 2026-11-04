//! Configuration sources are captured and resolved without changing process state.

use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Clone, Default)]
pub(crate) struct Environment {
    values: HashMap<OsString, OsString>,
    home: Option<PathBuf>,
}

impl Environment {
    pub(crate) fn capture() -> Self {
        Self {
            values: std::env::vars_os()
                .filter(|(key, _)| {
                    key.to_string_lossy().starts_with("REP_")
                        || matches!(
                            key.to_str(),
                            Some("HOME" | "USERPROFILE" | "XDG_CONFIG_HOME" | "NO_COLOR")
                        )
                })
                .collect(),
            // Preserve the standard library's passwd/profile fallback without
            // consulting the user's environment again while resolving config.
            home: std::env::home_dir(),
        }
    }

    pub(crate) fn no_color(&self) -> bool {
        self.get("NO_COLOR").is_some_and(|value| !value.is_empty())
    }

    pub(crate) fn get(&self, key: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(key)).map(OsString::as_os_str)
    }

    #[cfg(test)]
    pub(crate) fn from_pairs(values: &[(&str, &str)]) -> Self {
        Self::from_pairs_with_home(values, None)
    }

    #[cfg(test)]
    fn from_pairs_with_home(values: &[(&str, &str)], fallback: Option<PathBuf>) -> Self {
        // Injected snapshots must never fall back to the real user's home.
        let mut snapshot = Self {
            values: values
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect(),
            home: None,
        };
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        snapshot.home = snapshot
            .get(key)
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or(fallback);
        snapshot
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Source {
    Config,
    Environment,
}

#[derive(Default)]
pub(crate) struct Origin {
    pub(crate) sources: HashMap<String, Source>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct Config {
    pub hidden: Option<bool>,
    pub no_ignore: Option<bool>,
    pub ignore_case: Option<bool>,
    pub regex: Option<bool>,
    pub multiline: Option<bool>,
    pub dotall: Option<bool>,
    pub greedy: Option<bool>,
    pub word_regexp: Option<bool>,
    pub line_regexp: Option<bool>,
    pub smart: Option<bool>,
    pub preserve: Option<bool>,
    pub dry_run: Option<bool>,
    pub write: Option<bool>,
    pub preview: Option<bool>,
    pub preview_tool: Option<String>,
    pub context: Option<u64>,
    pub color: Option<String>,
    pub hyperlink_format: Option<String>,
    pub hyperlink_limit: Option<u64>,
    pub quiet: Option<bool>,
    pub hints: Option<bool>,
    pub style_added: Option<String>,
    pub style_removed: Option<String>,
    pub style_line_added: Option<String>,
    pub style_line_removed: Option<String>,
    pub marker_added: Option<String>,
    pub marker_removed: Option<String>,
}

impl Config {
    pub(crate) fn load(env: &Environment) -> Self {
        let Some(path) = resolve_path(env) else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(body) => match toml::from_str(&body) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("rep: {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("rep: failed to read {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub(crate) fn values(&self) -> HashMap<&'static str, OsString> {
        let cfg = self;
        let mut values = HashMap::new();
        set_bool(cfg.hidden, "REP_HIDDEN", &mut values);
        set_bool(cfg.no_ignore, "REP_NO_IGNORE", &mut values);
        set_bool(cfg.ignore_case, "REP_IGNORE_CASE", &mut values);
        set_bool(cfg.regex, "REP_REGEX", &mut values);
        set_bool(cfg.multiline, "REP_MULTILINE", &mut values);
        set_bool(cfg.dotall, "REP_DOTALL", &mut values);
        set_bool(cfg.greedy, "REP_GREEDY", &mut values);
        set_bool(cfg.word_regexp, "REP_WORD_REGEXP", &mut values);
        set_bool(cfg.line_regexp, "REP_LINE_REGEXP", &mut values);
        set_bool(cfg.smart, "REP_SMART", &mut values);
        set_bool(cfg.preserve, "REP_PRESERVE", &mut values);
        set_bool(cfg.dry_run, "REP_DRY_RUN", &mut values);
        set_bool(cfg.write, "REP_WRITE", &mut values);
        set_bool(cfg.preview, "REP_PREVIEW", &mut values);
        set_str(cfg.preview_tool.as_deref(), "REP_PREVIEW_TOOL", &mut values);
        set_num(cfg.context, "REP_CONTEXT", &mut values);
        set_str(cfg.color.as_deref(), "REP_COLOR", &mut values);
        set_str(
            cfg.hyperlink_format.as_deref(),
            "REP_HYPERLINK_FORMAT",
            &mut values,
        );
        set_num(cfg.hyperlink_limit, "REP_HYPERLINK_LIMIT", &mut values);
        set_bool(cfg.quiet, "REP_QUIET", &mut values);
        set_bool(cfg.hints, "REP_HINTS", &mut values);
        set_str(cfg.style_added.as_deref(), "REP_STYLE_ADDED", &mut values);
        set_str(
            cfg.style_removed.as_deref(),
            "REP_STYLE_REMOVED",
            &mut values,
        );
        set_str(
            cfg.style_line_added.as_deref(),
            "REP_STYLE_LINE_ADDED",
            &mut values,
        );
        set_str(
            cfg.style_line_removed.as_deref(),
            "REP_STYLE_LINE_REMOVED",
            &mut values,
        );
        set_str(cfg.marker_added.as_deref(), "REP_MARKER_ADDED", &mut values);
        set_str(
            cfg.marker_removed.as_deref(),
            "REP_MARKER_REMOVED",
            &mut values,
        );
        values
    }
}

fn set_bool(value: Option<bool>, key: &'static str, values: &mut HashMap<&'static str, OsString>) {
    if let Some(v) = value {
        values.insert(key, OsString::from(if v { "true" } else { "false" }));
    }
}
fn set_str(value: Option<&str>, key: &'static str, values: &mut HashMap<&'static str, OsString>) {
    if let Some(v) = value {
        values.insert(key, OsString::from(v));
    }
}
fn set_num(value: Option<u64>, key: &'static str, values: &mut HashMap<&'static str, OsString>) {
    if let Some(v) = value {
        values.insert(key, OsString::from(v.to_string()));
    }
}

fn resolve_path(env: &Environment) -> Option<PathBuf> {
    if let Some(path) = env.get("REP_CONFIG_PATH") {
        return (!path.is_empty()).then(|| PathBuf::from(path));
    }
    if let Some(path) = env.get("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("rep/config.toml"));
    }
    env.home
        .as_ref()
        .map(|home| home.join(".config/rep/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::{Config, Environment, resolve_path};
    use std::path::PathBuf;

    #[test]
    fn injected_environment_does_not_consult_real_home() {
        assert!(resolve_path(&Environment::default()).is_none());
        assert!(resolve_path(&Environment::from_pairs(&[])).is_none());
    }

    #[test]
    fn missing_or_empty_home_uses_captured_fallback() {
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let fallback = PathBuf::from("fallback-home");
        for pairs in [&[][..], &[(home_key, "")][..]] {
            let env = Environment::from_pairs_with_home(pairs, Some(fallback.clone()));
            assert_eq!(
                resolve_path(&env),
                Some(fallback.join(".config/rep/config.toml"))
            );
        }
    }

    #[test]
    fn platform_home_precedes_captured_fallback() {
        let env = Environment::from_pairs_with_home(
            &[("HOME", "unix-home"), ("USERPROFILE", "windows-home")],
            Some(PathBuf::from("fallback-home")),
        );
        let expected = if cfg!(windows) {
            "windows-home"
        } else {
            "unix-home"
        };
        assert_eq!(
            resolve_path(&env),
            Some(PathBuf::from(expected).join(".config/rep/config.toml"))
        );
    }

    #[test]
    fn explicit_config_and_xdg_override_home_fallback() {
        let home = Some(PathBuf::from("fallback-home"));
        let env = Environment::from_pairs_with_home(&[("XDG_CONFIG_HOME", "xdg")], home.clone());
        assert_eq!(
            resolve_path(&env),
            Some(PathBuf::from("xdg/rep/config.toml"))
        );
        let env = Environment::from_pairs_with_home(
            &[
                ("REP_CONFIG_PATH", "chosen.toml"),
                ("XDG_CONFIG_HOME", "xdg"),
            ],
            home.clone(),
        );
        assert_eq!(resolve_path(&env), Some(PathBuf::from("chosen.toml")));
        let env = Environment::from_pairs_with_home(
            &[("REP_CONFIG_PATH", ""), ("XDG_CONFIG_HOME", "xdg")],
            home,
        );
        assert!(resolve_path(&env).is_none());
    }

    #[test]
    fn no_color_uses_snapshot_presence_and_nonempty_value() {
        assert!(!Environment::default().no_color());
        assert!(!Environment::from_pairs(&[("NO_COLOR", "")]).no_color());
        assert!(Environment::from_pairs(&[("NO_COLOR", "1")]).no_color());
        assert!(Environment::from_pairs(&[("NO_COLOR", "0")]).no_color());
    }

    fn parse(input: &str) -> Config {
        toml::from_str(input).expect("valid config")
    }

    #[test]
    fn empty_file_yields_default_config() {
        let cfg = parse("");
        assert!(cfg.hidden.is_none());
        assert!(cfg.preview_tool.is_none());
        assert!(cfg.context.is_none());
    }

    #[test]
    fn kebab_case_keys_map_to_snake_fields() {
        let cfg = parse(
            "\
hidden = true
ignore-case = true
preview-tool = \"delta\"
hyperlink-limit = 0
context = 5
",
        );
        assert_eq!(cfg.hidden, Some(true));
        assert_eq!(cfg.ignore_case, Some(true));
        assert_eq!(cfg.preview_tool.as_deref(), Some("delta"));
        assert_eq!(cfg.hyperlink_limit, Some(0));
        assert_eq!(cfg.context, Some(5));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<Config>("not-a-real-flag = true")
            .err()
            .expect("expected unknown-field error");
        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }
}

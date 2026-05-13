//! TOML configuration loader for `~/.config/rep/config.toml`.
//!
//! Each `Some` field is projected onto a matching `REP_*` env var (only when
//! the env var is not already set, so a user's shell env always beats the
//! config file). Clap reads those env vars natively via `#[arg(env = ...)]`.
//!
//! `load_into_env` returns an [`Origin`] recording which `REP_*` keys were
//! synthesized from config (vs already provided by the user's shell). The
//! resolver in `main` uses that record to enforce the full
//! `config < env < CLI` precedence: `ValueSource::EnvVariable` alone cannot
//! tell config-derived values apart from real shell env.
//!
//! Read and parse failures print to stderr and continue; they never abort.

use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;

const PATH_ENV: &str = "REP_CONFIG_PATH";

/// Records which `REP_*` env var names were synthesized by the config loader.
/// An entry being present means "this env var carries a config-derived value";
/// absent means "anything in the env for this key came from outside rep" (the
/// user's shell, a parent process, a wrapper script).
#[derive(Default)]
pub struct Origin {
    keys: HashSet<&'static str>,
}

impl Origin {
    pub fn is_config_derived(&self, env_name: &str) -> bool {
        self.keys.contains(env_name)
    }

    /// Remove every `REP_*` env var that this loader synthesized, so spawned
    /// subprocesses inherit only the user's shell env. Startup-only:
    /// must be called before any worker threads spawn.
    pub fn unset_synthesized(&self) {
        for key in &self.keys {
            // Single-threaded startup path.
            unsafe {
                std::env::remove_var(key);
            }
        }
    }

    /// Mark `env_name` as config-derived without actually setting the env
    /// var. Intended for tests that simulate config projection by setting
    /// env vars directly.
    #[cfg(test)]
    pub fn mark_as_config_derived(&mut self, env_name: &'static str) {
        self.keys.insert(env_name);
    }
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

/// Resolve the config path, parse it if present, project each set field
/// onto the matching `REP_*` env var, and return an [`Origin`] recording
/// which env vars were synthesized. The env layer is set-once via
/// `set_if_unset`: any value already in the environment (from the user's
/// shell) wins over the config file, and only the keys we successfully
/// set get recorded in `Origin`.
pub fn load_into_env() -> Origin {
    let mut origin = Origin::default();
    let Some(path) = resolve_path() else {
        return origin;
    };
    if !path.exists() {
        return origin;
    }
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("rep: failed to read {}: {err}", path.display());
            return origin;
        }
    };
    let cfg: Config = match toml::from_str(&body) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("rep: {}: {err}", path.display());
            return origin;
        }
    };
    apply_to_env(&cfg, &mut origin);
    origin
}

fn apply_to_env(cfg: &Config, origin: &mut Origin) {
    set_bool(cfg.hidden, "REP_HIDDEN", origin);
    set_bool(cfg.no_ignore, "REP_NO_IGNORE", origin);
    set_bool(cfg.ignore_case, "REP_IGNORE_CASE", origin);
    set_bool(cfg.regex, "REP_REGEX", origin);
    set_bool(cfg.multiline, "REP_MULTILINE", origin);
    set_bool(cfg.dotall, "REP_DOTALL", origin);
    set_bool(cfg.greedy, "REP_GREEDY", origin);
    set_bool(cfg.word_regexp, "REP_WORD_REGEXP", origin);
    set_bool(cfg.line_regexp, "REP_LINE_REGEXP", origin);
    set_bool(cfg.smart, "REP_SMART", origin);
    set_bool(cfg.preserve, "REP_PRESERVE", origin);
    set_bool(cfg.dry_run, "REP_DRY_RUN", origin);
    set_bool(cfg.write, "REP_WRITE", origin);
    set_bool(cfg.preview, "REP_PREVIEW", origin);
    set_str(cfg.preview_tool.as_deref(), "REP_PREVIEW_TOOL", origin);
    set_num(cfg.context, "REP_CONTEXT", origin);
    set_str(cfg.color.as_deref(), "REP_COLOR", origin);
    set_str(
        cfg.hyperlink_format.as_deref(),
        "REP_HYPERLINK_FORMAT",
        origin,
    );
    set_num(cfg.hyperlink_limit, "REP_HYPERLINK_LIMIT", origin);
    set_bool(cfg.quiet, "REP_QUIET", origin);
    set_bool(cfg.hints, "REP_HINTS", origin);
    set_str(cfg.style_added.as_deref(), "REP_STYLE_ADDED", origin);
    set_str(cfg.style_removed.as_deref(), "REP_STYLE_REMOVED", origin);
    set_str(
        cfg.style_line_added.as_deref(),
        "REP_STYLE_LINE_ADDED",
        origin,
    );
    set_str(
        cfg.style_line_removed.as_deref(),
        "REP_STYLE_LINE_REMOVED",
        origin,
    );
    set_str(cfg.marker_added.as_deref(), "REP_MARKER_ADDED", origin);
    set_str(cfg.marker_removed.as_deref(), "REP_MARKER_REMOVED", origin);
}

fn set_bool(value: Option<bool>, key: &'static str, origin: &mut Origin) {
    if let Some(v) = value {
        set_if_unset(key, if v { "true" } else { "false" }, origin);
    }
}

fn set_str(value: Option<&str>, key: &'static str, origin: &mut Origin) {
    if let Some(v) = value {
        set_if_unset(key, v, origin);
    }
}

fn set_num(value: Option<u64>, key: &'static str, origin: &mut Origin) {
    if let Some(v) = value {
        set_if_unset(key, &v.to_string(), origin);
    }
}

fn set_if_unset(key: &'static str, value: &str, origin: &mut Origin) {
    if std::env::var_os(key).is_some() {
        return;
    }
    // Single-threaded startup path.
    unsafe {
        std::env::set_var(key, value);
    }
    origin.keys.insert(key);
}

fn resolve_path() -> Option<PathBuf> {
    // An explicitly-set `REP_CONFIG_PATH` always wins, even if it points
    // somewhere that doesn't exist - that's how the user disables the
    // default lookup. An empty value means "no config".
    if let Some(value) = std::env::var_os(PATH_ENV) {
        if value.is_empty() {
            return None;
        }
        return Some(PathBuf::from(value));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("rep/config.toml"));
    }
    #[allow(deprecated)]
    let home = std::env::home_dir()?;
    Some(home.join(".config/rep/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::Config;

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

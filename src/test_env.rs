//! Shared test helpers for env-isolated CLI parsing.
//!
//! The mutex only synchronises cooperating callers (via [`lock_for_parse`]
//! or [`EnvGuard`]). Unrelated env reads in the test binary remain a
//! theoretical race; in practice the binary's only env reads go through
//! this module.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Strip every `REP_*` env var. Runs once per test binary. Private so the
/// only callers are [`lock_for_parse`] and [`EnvGuard::set`], both of
/// which acquire `ENV_MUTEX` first.
fn clear_rep_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let to_remove: Vec<_> = std::env::vars_os()
            .filter(|(k, _)| k.to_string_lossy().starts_with("REP_"))
            .map(|(k, _)| k)
            .collect();
        for key in to_remove {
            // First env mutation in the binary; gated by `Once`.
            unsafe {
                std::env::remove_var(&key);
            }
        }
    });
}

/// Acquire [`ENV_MUTEX`] and clear ambient `REP_*` env vars. Guard must
/// outlive any clap parse the caller performs.
pub fn lock_for_parse() -> MutexGuard<'static, ()> {
    let lock = ENV_MUTEX.lock().unwrap_or_else(PoisonError::into_inner);
    clear_rep_env();
    lock
}

/// Sets env vars for the guard's lifetime and restores them on drop. Only
/// the vars passed to `set` are restored; ambient `REP_*` vars cleared by
/// the initial scrub stay cleared.
pub struct EnvGuard {
    prior: Vec<(OsString, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn set(vars: &[(&'static str, &'static str)]) -> Self {
        let lock = ENV_MUTEX.lock().unwrap_or_else(PoisonError::into_inner);
        clear_rep_env();
        let prior = vars
            .iter()
            .map(|(k, _)| ((*k).into(), std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            // Holding `ENV_MUTEX` via `lock`.
            unsafe {
                std::env::set_var(k, v);
            }
        }
        Self { prior, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prior) in &self.prior {
            // `_lock` drops after `prior` (declaration order), so the
            // mutex is still held here.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

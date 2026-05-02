//! ANSI styling helpers with automatic TTY fallback.
//!
//! When stdout is a tty, helpers wrap text in ANSI escape codes; when
//! piped or redirected they return the input unchanged so logs and CI
//! captures stay clean.

use std::{
    io::{IsTerminal, stdout},
    sync::OnceLock,
};

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if std::env::var("KITHARA_AUDIT_COLOR")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("always"))
            .unwrap_or(false)
        {
            return true;
        }
        stdout().is_terminal()
    })
}

fn wrap(prefix: &str, body: &str) -> String {
    if enabled() {
        format!("\x1b[{prefix}m{body}\x1b[0m")
    } else {
        body.to_string()
    }
}

pub(crate) fn bold(s: &str) -> String {
    wrap("1", s)
}

pub(crate) fn dim(s: &str) -> String {
    wrap("2", s)
}

pub(crate) fn red(s: &str) -> String {
    wrap("31", s)
}

#[expect(dead_code, reason = "kept for future severity tags")]
pub(crate) fn yellow(s: &str) -> String {
    wrap("33", s)
}

pub(crate) fn cyan(s: &str) -> String {
    wrap("36", s)
}

pub(crate) fn green(s: &str) -> String {
    wrap("32", s)
}

pub(crate) fn bold_red(s: &str) -> String {
    wrap("1;31", s)
}

pub(crate) fn bold_yellow(s: &str) -> String {
    wrap("1;33", s)
}

pub(crate) fn bold_cyan(s: &str) -> String {
    wrap("1;36", s)
}

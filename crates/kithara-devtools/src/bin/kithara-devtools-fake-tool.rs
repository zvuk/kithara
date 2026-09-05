//! A stand-in for a quality-lab tool, for the tests of `quality_lab::execution`.
//!
//! It is a real program because the code under test captures a child's stdout
//! into a file and parses that file as the tool's report: a libtest child would
//! frame its own stdout and the report would never parse. Every byte it writes
//! to stdout is the report, which is why it writes bytes rather than logging.
//!
//! Its behaviour is a file beside its own executable, named by appending
//! `.behaviour`: first line the version it answers `--version` with, second
//! line the exit code for any other invocation, and the rest the report. Each
//! test copies this program into its own directory, so nothing is shared.

use std::{
    env, fs,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let Ok(executable) = env::current_exe() else {
        return fail("locating this executable");
    };
    let behaviour = executable.with_extension("behaviour");
    let Ok(text) = fs::read_to_string(&behaviour) else {
        return fail(&format!("reading {}", behaviour.display()));
    };
    let mut lines = text.splitn(3, '\n');
    let (Some(version), Some(code), Some(report)) = (lines.next(), lines.next(), lines.next())
    else {
        return fail("the behaviour file needs a version, an exit code and a report");
    };

    if env::args().nth(1).as_deref() == Some("--version") {
        return match writeln!(io::stdout(), "{version}") {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => fail("writing the version"),
        };
    }
    if write!(io::stdout(), "{report}").is_err() {
        return fail("writing the report");
    }
    let Ok(code) = code.trim().parse::<u8>() else {
        return fail("the behaviour file's second line is not an exit code");
    };
    ExitCode::from(code)
}

fn fail(what: &str) -> ExitCode {
    let _ = writeln!(io::stderr(), "kithara-devtools-fake-tool: {what}");
    ExitCode::from(2u8)
}

//! Test double for the external programs `xtask` spawns.
//!
//! One binary, many roles: the role is the file name it was invoked under, so
//! a test copies it to the path the code under test will reach — `<tmp>/bin/
//! sccache`, `<brew_root>/bin/colima` — and nothing about the production path
//! changes to accommodate the test.
//!
//! It is a program rather than an ignored test because these call sites choose
//! their own arguments: libtest would reject `--stop-server` before the double
//! could answer it.

use std::{env, fs::OpenOptions, io::Write as _, path::Path, process::ExitCode};

/// The environment this double reads its whole behaviour from.
struct Consts;

impl Consts {
    /// File the arguments of each run are appended to, one line per run,
    /// joined by spaces. The whole line, not the first argument: `colima`
    /// reaches `fstrim` as its sixth argument, and `launchctl bootout
    /// <service>` is asserted in full today.
    const TRACE: &str = "KITHARA_TEST_TRACE";
    /// Scenario selector, read together with the role and the first argument.
    const SCENARIO: &str = "KITHARA_TEST_SCENARIO";
    /// `role:first-argument:scenario=exit_code` triples, separated by commas.
    /// The first matching triple decides the exit code; `*` matches anything.
    const RULES: &str = "KITHARA_TEST_RULES";
    /// Text printed to stdout before exiting, verbatim.
    const STDOUT: &str = "KITHARA_TEST_STDOUT";
    /// Text printed to stderr before exiting, verbatim.
    const STDERR: &str = "KITHARA_TEST_STDERR";
}

fn main() -> ExitCode {
    let role = env::args_os()
        .next()
        .map(|argv0| {
            Path::new(&argv0)
                .file_stem()
                .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let args: Vec<String> = env::args().skip(1).collect();
    let first = args.first().map_or("", String::as_str);
    let scenario = env::var(Consts::SCENARIO).unwrap_or_default();

    if let Some(path) = env::var_os(Consts::TRACE) {
        let opened = OpenOptions::new().create(true).append(true).open(path);
        match opened {
            Ok(mut file) => {
                if writeln!(file, "{}", args.join(" ")).is_err() {
                    return fail("appending to the trace file");
                }
            }
            Err(_) => return fail("opening the trace file"),
        }
    }
    if let Some(text) = env::var_os(Consts::STDOUT) {
        print!("{}", text.to_string_lossy());
    }
    if let Some(text) = env::var_os(Consts::STDERR) {
        eprint!("{}", text.to_string_lossy());
    }
    ExitCode::from(exit_code(
        &env::var(Consts::RULES).unwrap_or_default(),
        &role,
        first,
        &scenario,
    ))
}

fn fail(what: &str) -> ExitCode {
    eprintln!("fake-tool: {what}");
    ExitCode::from(2u8)
}

/// First matching `role:argument:scenario=code` rule wins; `*` matches
/// anything; no match is success.
fn exit_code(rules: &str, role: &str, argument: &str, scenario: &str) -> u8 {
    for rule in rules.split(',').filter(|rule| !rule.is_empty()) {
        let Some((pattern, code)) = rule.split_once('=') else {
            continue;
        };
        let mut parts = pattern.split(':');
        let matches = [role, argument, scenario].into_iter().all(|actual| {
            parts
                .next()
                .is_none_or(|want| want == "*" || want == actual)
        });
        if matches {
            return code.parse().unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::exit_code;

    #[test]
    fn the_first_matching_rule_wins() {
        let rules = "sccache:--stop-server:stop-failure=7,sccache:*:*=0";

        assert_eq!(
            exit_code(rules, "sccache", "--stop-server", "stop-failure"),
            7
        );
        assert_eq!(exit_code(rules, "sccache", "--stop-server", "success"), 0);
        assert_eq!(exit_code(rules, "colima", "fstrim", "success"), 0);
    }

    #[test]
    fn an_empty_rule_set_succeeds() {
        assert_eq!(exit_code("", "anything", "anything", "anything"), 0);
    }
}

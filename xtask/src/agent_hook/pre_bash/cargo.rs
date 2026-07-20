use super::shell::argv0;

pub(super) fn cargo_subcommand(tokens: &[String]) -> Option<(&str, &[String])> {
    if argv0(tokens)? != "cargo" {
        return None;
    }
    let mut idx = 1;
    if tokens.get(idx).is_some_and(|arg| arg.starts_with('+')) {
        idx += 1;
    }
    while let Some(arg) = tokens.get(idx) {
        if arg == "--" {
            idx += 1;
            break;
        }
        if cargo_global_option_takes_value(arg) {
            idx += 2;
            continue;
        }
        if cargo_global_option_has_value(arg) || arg.starts_with('-') {
            idx += 1;
            continue;
        }
        break;
    }
    let subcommand = tokens.get(idx)?.as_str();
    Some((subcommand, &tokens[idx + 1..]))
}

fn cargo_global_option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "--color"
            | "--config"
            | "--manifest-path"
            | "--message-format"
            | "--target"
            | "--target-dir"
            | "-C"
            | "-Z"
            | "-j"
            | "--jobs"
    )
}

fn cargo_global_option_has_value(arg: &str) -> bool {
    arg.starts_with("--color=")
        || arg.starts_with("--config=")
        || arg.starts_with("--manifest-path=")
        || arg.starts_with("--message-format=")
        || arg.starts_with("--target=")
        || arg.starts_with("--target-dir=")
        || ((arg.starts_with("-C") || arg.starts_with("-Z") || arg.starts_with("-j"))
            && arg.len() > 2)
}

pub(super) fn is_full_test_harness(tokens: &[String]) -> bool {
    match argv0(tokens) {
        Some("just") => tokens.get(1).is_some_and(|arg| arg.starts_with("test")),
        Some("cargo") => {
            let Some((subcommand, rest)) = cargo_subcommand(tokens) else {
                return false;
            };
            subcommand == "xtask" && rest.first().is_some_and(|arg| arg == "test")
        }
        _ => false,
    }
}

pub(super) fn is_broad_cargo_test(args: &[String]) -> bool {
    !has_scoping_flag(args) && !has_test_filter(args)
}

pub(super) fn is_broad_nextest(args: &[String]) -> bool {
    let Some((run_idx, _)) = args.iter().enumerate().find(|(_, arg)| *arg == "run") else {
        return false;
    };
    let run_args = &args[run_idx + 1..];
    !has_nextest_scope(run_args)
}

fn has_scoping_flag(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-m" | "--manifest-path"
                | "-p"
                | "--package"
                | "--test"
                | "--tests"
                | "--bin"
                | "--bins"
                | "--lib"
                | "--example"
                | "--examples"
                | "--bench"
                | "--benches"
        ) || arg.starts_with("--manifest-path=")
            || arg.starts_with("--package=")
            || arg.starts_with("--test=")
            || arg.starts_with("--bin=")
            || arg.starts_with("--example=")
            || arg.starts_with("--bench=")
            || ((arg.starts_with("-m") || arg.starts_with("-p")) && arg.len() > 2)
    })
}

fn has_nextest_scope(args: &[String]) -> bool {
    let mut idx = 0;
    while let Some(arg) = args.get(idx) {
        if has_scoping_flag(&args[idx..=idx])
            || matches!(arg.as_str(), "-E" | "--filter-expr" | "--filterset")
            || (arg.starts_with("-E") && arg.len() > 2)
            || arg.starts_with("--filter-expr=")
            || arg.starts_with("--filterset=")
        {
            return true;
        }
        if arg == "--" {
            return has_harness_filter(&args[idx + 1..]);
        }
        if nextest_option_takes_value(arg) {
            idx += 2;
            continue;
        }
        if nextest_option_has_value(arg) || arg.starts_with('-') {
            idx += 1;
            continue;
        }
        return true;
    }
    false
}

fn nextest_option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "--archive-file"
            | "--archive-format"
            | "--binaries-metadata"
            | "--build-jobs"
            | "--cargo-profile"
            | "--cargo-metadata"
            | "--color"
            | "--config"
            | "--config-file"
            | "--exclude"
            | "--extract-to"
            | "--failure-output"
            | "--final-status-level"
            | "-F"
            | "--features"
            | "--max-fail"
            | "--message-format"
            | "--message-format-version"
            | "--no-tests"
            | "-P"
            | "--partition"
            | "--platform-filter"
            | "--profile"
            | "--retries"
            | "--run-ignored"
            | "--show-progress"
            | "--status-level"
            | "--stress-count"
            | "--stress-duration"
            | "--success-output"
            | "--target"
            | "--target-dir"
            | "--target-dir-remap"
            | "--test-threads"
            | "--tool-config-file"
            | "--workspace-remap"
            | "-Z"
            | "-j"
            | "--jobs"
    )
}

fn nextest_option_has_value(arg: &str) -> bool {
    arg.starts_with("--archive-file=")
        || arg.starts_with("--archive-format=")
        || arg.starts_with("--binaries-metadata=")
        || arg.starts_with("--build-jobs=")
        || arg.starts_with("--cargo-profile=")
        || arg.starts_with("--cargo-metadata=")
        || arg.starts_with("--color=")
        || arg.starts_with("--config=")
        || arg.starts_with("--config-file=")
        || arg.starts_with("--exclude=")
        || arg.starts_with("--extract-to=")
        || arg.starts_with("--failure-output=")
        || arg.starts_with("--final-status-level=")
        || arg.starts_with("--features=")
        || arg.starts_with("--max-fail=")
        || arg.starts_with("--message-format=")
        || arg.starts_with("--message-format-version=")
        || arg.starts_with("--no-tests=")
        || arg.starts_with("--partition=")
        || arg.starts_with("--platform-filter=")
        || arg.starts_with("--profile=")
        || arg.starts_with("--retries=")
        || arg.starts_with("--run-ignored=")
        || arg.starts_with("--show-progress=")
        || arg.starts_with("--status-level=")
        || arg.starts_with("--stress-count=")
        || arg.starts_with("--stress-duration=")
        || arg.starts_with("--success-output=")
        || arg.starts_with("--target=")
        || arg.starts_with("--target-dir=")
        || arg.starts_with("--target-dir-remap=")
        || arg.starts_with("--test-threads=")
        || arg.starts_with("--tool-config-file=")
        || arg.starts_with("--workspace-remap=")
        || ((arg.starts_with("-F")
            || arg.starts_with("-P")
            || arg.starts_with("-Z")
            || arg.starts_with("-j"))
            && arg.len() > 2)
}

fn has_test_filter(args: &[String]) -> bool {
    let mut idx = 0;
    while let Some(arg) = args.get(idx) {
        if arg == "--" {
            return has_harness_filter(&args[idx + 1..]);
        }
        if test_option_takes_value(arg) {
            idx += 2;
            continue;
        }
        if test_option_has_value(arg) || arg.starts_with('-') {
            idx += 1;
            continue;
        }
        return true;
    }
    false
}

fn has_harness_filter(args: &[String]) -> bool {
    let mut idx = 0;
    while let Some(arg) = args.get(idx) {
        if matches!(arg.as_str(), "--format" | "--skip" | "--test-threads") {
            idx += 2;
            continue;
        }
        if arg.starts_with("--format=")
            || arg.starts_with("--skip=")
            || arg.starts_with("--test-threads=")
            || arg.starts_with('-')
        {
            idx += 1;
            continue;
        }
        if !arg.is_empty() {
            return true;
        }
        idx += 1;
    }
    false
}

fn test_option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "--color"
            | "--config"
            | "--exclude"
            | "-F"
            | "--profile"
            | "--features"
            | "--message-format"
            | "--target"
            | "--target-dir"
            | "-Z"
            | "-j"
            | "--jobs"
    )
}

fn test_option_has_value(arg: &str) -> bool {
    arg.starts_with("--color=")
        || arg.starts_with("--config=")
        || arg.starts_with("--exclude=")
        || arg.starts_with("--features=")
        || arg.starts_with("--message-format=")
        || arg.starts_with("--profile=")
        || arg.starts_with("--target=")
        || arg.starts_with("--target-dir=")
        || ((arg.starts_with("-F") || arg.starts_with("-Z") || arg.starts_with("-j"))
            && arg.len() > 2)
}

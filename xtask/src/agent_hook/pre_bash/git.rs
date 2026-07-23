use super::shell::{argv0, is_rtk_option};

pub(super) fn is_destructive_git(tokens: &[String]) -> bool {
    let Some((subcommand, args)) = git_subcommand(tokens) else {
        return false;
    };
    match subcommand {
        "clean" => true,
        "reset" => args.iter().any(|arg| arg == "--hard"),
        "checkout" => args.iter().any(|arg| arg == "-f" || arg == "--"),
        "restore" => args.iter().any(|arg| arg == "." || arg == ":/"),
        "branch" => args.iter().any(|arg| arg == "-D"),
        "push" => args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--force" | "-f" | "--force-with-lease")),
        _ => false,
    }
}

fn git_subcommand(tokens: &[String]) -> Option<(&str, &[String])> {
    if argv0(tokens)? != "git" {
        return None;
    }
    let mut idx = 1;
    while let Some(arg) = tokens.get(idx) {
        if arg == "--" {
            idx += 1;
            break;
        }
        if is_rtk_option(arg) {
            idx += 1;
            continue;
        }
        if matches!(
            arg.as_str(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            idx += 2;
            continue;
        }
        if ((arg.starts_with("-C") || arg.starts_with("-c")) && arg.len() > 2)
            || arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--config-env=")
            || matches!(
                arg.as_str(),
                "--bare" | "--literal-pathspecs" | "--no-optional-locks" | "--no-pager"
            )
        {
            idx += 1;
            continue;
        }
        break;
    }
    let subcommand = tokens.get(idx)?.as_str();
    Some((subcommand, &tokens[idx + 1..]))
}

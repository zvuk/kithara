use std::path::Path;

pub(super) enum NormalizedCommand {
    Args(Vec<String>),
    Shell(String),
}

pub(super) fn normalize_command(tokens: &[String]) -> NormalizedCommand {
    let tokens = strip_env_assignments(tokens);
    match argv0(tokens) {
        Some("env") => return normalize_env(&tokens[1..]),
        Some("rtk") => {}
        _ => return NormalizedCommand::Args(tokens.to_vec()),
    }

    let tokens = strip_rtk_options(&tokens[1..]);
    let Some(wrapper) = tokens.first() else {
        return NormalizedCommand::Args(Vec::new());
    };
    let args = strip_rtk_options(&tokens[1..]);
    match wrapper.as_str() {
        "env" => normalize_env(args),
        "err" | "proxy" | "summary" | "test" => normalize_command_wrapper(args),
        "run" => normalize_rtk_run(args),
        _ => {
            let mut normalized = Vec::with_capacity(args.len() + 1);
            normalized.push(wrapper.clone());
            normalized.extend_from_slice(args);
            NormalizedCommand::Args(normalized)
        }
    }
}

fn normalize_command_wrapper(tokens: &[String]) -> NormalizedCommand {
    let tokens = strip_env_assignments(tokens);
    match tokens {
        [command] => NormalizedCommand::Shell(command.clone()),
        _ => NormalizedCommand::Args(tokens.to_vec()),
    }
}

fn normalize_env(tokens: &[String]) -> NormalizedCommand {
    let mut idx = 0;
    while let Some(option) = tokens.get(idx) {
        if option == "--" {
            idx += 1;
            break;
        }
        if is_rtk_option(option)
            || matches!(
                option.as_str(),
                "-" | "-0" | "-i" | "--ignore-environment" | "--null"
            )
        {
            idx += 1;
            continue;
        }
        if matches!(
            option.as_str(),
            "-u" | "--unset" | "-C" | "--chdir" | "-P" | "--path"
        ) {
            idx += 2;
            continue;
        }
        if matches!(option.as_str(), "-S" | "--split-string") {
            return tokens.get(idx + 1).map_or_else(
                || NormalizedCommand::Args(Vec::new()),
                |command| shell_command_with_rest(command, &tokens[idx + 2..]),
            );
        }
        if let Some(command) = option
            .strip_prefix("-S")
            .filter(|command| !command.is_empty())
            .or_else(|| option.strip_prefix("--split-string="))
        {
            return shell_command_with_rest(command, &tokens[idx + 1..]);
        }
        if ((option.starts_with("-u") || option.starts_with("-C") || option.starts_with("-P"))
            && option.len() > 2)
            || option.starts_with("--unset=")
            || option.starts_with("--chdir=")
            || option.starts_with("--path=")
        {
            idx += 1;
            continue;
        }
        break;
    }
    normalize_command_wrapper(strip_env_assignments(&tokens[idx..]))
}

fn shell_command_with_rest(command: &str, rest: &[String]) -> NormalizedCommand {
    if rest.is_empty() {
        return NormalizedCommand::Shell(command.to_owned());
    }
    NormalizedCommand::Shell(format!("{command} {}", rest.join(" ")))
}

fn normalize_rtk_run(tokens: &[String]) -> NormalizedCommand {
    match tokens.first().map(String::as_str) {
        Some("-c" | "--command") => tokens.get(1).map_or_else(
            || NormalizedCommand::Args(Vec::new()),
            |command| NormalizedCommand::Shell(command.clone()),
        ),
        Some(option) if option.starts_with("--command=") => NormalizedCommand::Shell(
            option
                .strip_prefix("--command=")
                .unwrap_or_default()
                .to_owned(),
        ),
        Some(_) => NormalizedCommand::Shell(tokens.join(" ")),
        None => NormalizedCommand::Args(Vec::new()),
    }
}

fn strip_rtk_options(tokens: &[String]) -> &[String] {
    let mut start = 0;
    while let Some(token) = tokens.get(start) {
        if token == "--" {
            start += 1;
            break;
        }
        if is_rtk_option(token) {
            start += 1;
            continue;
        }
        break;
    }
    &tokens[start..]
}

pub(super) fn is_rtk_option(token: &str) -> bool {
    matches!(token, "--skip-env" | "--ultra-compact" | "--verbose")
        || token.starts_with("--verbose=")
        || token
            .strip_prefix('-')
            .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch == 'v'))
}

pub(super) fn shell_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ' ' | '\t' => push_token(&mut tokens, &mut current),
                '\n' | ';' => {
                    push_token(&mut tokens, &mut current);
                    tokens.push(";".to_owned());
                }
                '&' | '|' => {
                    push_token(&mut tokens, &mut current);
                    if chars.peek() == Some(&ch) {
                        chars.next();
                        tokens.push(format!("{ch}{ch}"));
                    } else {
                        tokens.push(ch.to_string());
                    }
                }
                _ => current.push(ch),
            },
        }
    }
    push_token(&mut tokens, &mut current);
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

pub(super) fn command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (idx, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), ";" | "&" | "&&" | "||" | "|") {
            if start < idx {
                segments.push(&tokens[start..idx]);
            }
            start = idx + 1;
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn strip_env_assignments(tokens: &[String]) -> &[String] {
    let mut start = 0;
    while let Some(token) = tokens.get(start) {
        if is_env_assignment(token) {
            start += 1;
        } else {
            break;
        }
    }
    &tokens[start..]
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.contains('/')
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && !name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

pub(super) fn strip_timeout_wrapper(tokens: &[String]) -> Option<&[String]> {
    if !matches!(argv0(tokens)?, "timeout" | "gtimeout") {
        return None;
    }
    let mut idx = 1;
    while let Some(token) = tokens.get(idx) {
        if token == "--" {
            idx += 1;
            break;
        }
        if matches!(token.as_str(), "-k" | "--kill-after") {
            idx += 2;
            continue;
        }
        if token.starts_with("--kill-after=") {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        idx += 1;
        break;
    }
    (idx < tokens.len()).then_some(&tokens[idx..])
}

pub(super) fn argv0(tokens: &[String]) -> Option<&str> {
    let first = tokens.first()?;
    let name = Path::new(first).file_name()?.to_str()?;
    Some(name.trim_end_matches(".exe"))
}

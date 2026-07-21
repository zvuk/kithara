use std::{env, fs};

use anyhow::{Context, Result};

use crate::config::{HookEvent, HookHandler, HookToolKind, KitharaExt};

mod input;
mod post_edit;
mod pre_bash;

pub(crate) fn run() -> Result<()> {
    let root = env::current_dir().context("resolve agent hook working directory")?;
    let root = fs::canonicalize(&root)
        .with_context(|| format!("resolve agent hook root {}", root.display()))?;
    let ext = KitharaExt::load(&root)?;
    let config = ext.agent_hook()?;
    let input = input::read()?;

    let Some(event) = normalize_event(&input.hook_event_name) else {
        return Ok(());
    };
    let Some(tool_kind) = normalize_tool(&input.tool_name) else {
        return Ok(());
    };
    let Some(route) = config
        .routes
        .iter()
        .find(|route| route.event == event && route.tool_kind == tool_kind)
    else {
        return Ok(());
    };

    match route.handler {
        HookHandler::CommandGuard => pre_bash::run(
            &input.hook_event_name,
            input.string_field("command")?,
            input.has_field("timeout")?,
            &config.destructive_git_override_env,
        ),
        HookHandler::FormatEditedPaths => post_edit::run(&input, &root),
    }
}

fn normalize_event(name: &str) -> Option<HookEvent> {
    match name {
        "PreToolUse" => Some(HookEvent::PreToolUse),
        "PostToolUse" => Some(HookEvent::PostToolUse),
        _ => None,
    }
}

fn normalize_tool(name: &str) -> Option<HookToolKind> {
    match name {
        "Bash" => Some(HookToolKind::Shell),
        "Write" | "Edit" | "MultiEdit" | "apply_patch" => Some(HookToolKind::FileEdit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_event, normalize_tool};
    use crate::config::{HookEvent, HookToolKind};

    #[test]
    fn normalizes_supported_provider_discriminators() {
        assert_eq!(normalize_event("PreToolUse"), Some(HookEvent::PreToolUse));
        assert_eq!(normalize_event("PostToolUse"), Some(HookEvent::PostToolUse));
        assert_eq!(normalize_tool("Bash"), Some(HookToolKind::Shell));
        for tool in ["Write", "Edit", "MultiEdit", "apply_patch"] {
            assert_eq!(normalize_tool(tool), Some(HookToolKind::FileEdit));
        }
    }

    #[test]
    fn leaves_unknown_provider_discriminators_unrouted() {
        assert_eq!(normalize_event("Stop"), None);
        assert_eq!(normalize_tool("Read"), None);
    }
}

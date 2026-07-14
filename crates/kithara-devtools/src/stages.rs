use crate::common::scope::{Scope, Tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedStage {
    FmtCheck,
    Clippy,
    AstGrep,
    Lint,
    Typos,
    Similarity,
    Orphans,
}

impl SharedStage {
    pub(crate) const AUDIT: [Self; 7] = [
        Self::FmtCheck,
        Self::Clippy,
        Self::AstGrep,
        Self::Lint,
        Self::Typos,
        Self::Similarity,
        Self::Orphans,
    ];

    pub(crate) fn audit_command(self, scope: &Scope) -> StageCommand {
        let (program, mut args) = match self {
            Self::FmtCheck => ("cargo", strings(&["+nightly", "fmt"])),
            Self::Clippy => ("cargo", strings(&["clippy", "--quiet"])),
            Self::AstGrep => ("xtask", strings(&["ast-grep"])),
            Self::Lint => ("xtask", strings(&["lint"])),
            Self::Typos => ("xtask", strings(&["typos"])),
            Self::Similarity => ("xtask", strings(&["similarity", "--profile", "audit"])),
            Self::Orphans => ("xtask", strings(&["orphans", "--deny", "--audit-mode"])),
        };
        args.extend(scope.flags_for(self.scope_tool()));
        match self {
            Self::FmtCheck => args.push("--check".to_owned()),
            Self::Clippy => args.extend(strings(&["--", "-D", "warnings"])),
            _ => {}
        }
        StageCommand { program, args }
    }

    pub(crate) const fn audit_name(self) -> &'static str {
        match self {
            Self::FmtCheck => "fmt-check",
            Self::Clippy => "clippy",
            Self::AstGrep => "ast-grep",
            Self::Lint => "lint",
            Self::Typos => "typos",
            Self::Similarity => "similarity",
            Self::Orphans => "orphans",
        }
    }

    pub(crate) const fn audit_section(self) -> &'static str {
        match self {
            Self::Lint => "xtask lint",
            _ => self.audit_name(),
        }
    }

    pub(crate) fn health_command(self) -> StageCommand {
        let args = match self {
            Self::FmtCheck => strings(&["xtask", "format", "--check"]),
            Self::Clippy => strings(&["clippy", "--workspace", "--", "-D", "warnings"]),
            Self::AstGrep => strings(&["xtask", "ast-grep"]),
            Self::Lint => strings(&["xtask", "lint"]),
            Self::Typos => strings(&["xtask", "typos"]),
            Self::Similarity => strings(&["xtask", "similarity", "--profile=strict"]),
            Self::Orphans => strings(&["xtask", "orphans", "--deny"]),
        };
        StageCommand {
            args,
            program: "cargo",
        }
    }

    const fn scope_tool(self) -> Tool {
        match self {
            Self::FmtCheck => Tool::Fmt,
            Self::Clippy => Tool::Clippy,
            Self::AstGrep => Tool::AstGrep,
            Self::Lint => Tool::Xtask,
            Self::Typos => Tool::Typos,
            Self::Similarity => Tool::Similarity,
            Self::Orphans => Tool::Orphans,
        }
    }
}

pub(crate) struct StageCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

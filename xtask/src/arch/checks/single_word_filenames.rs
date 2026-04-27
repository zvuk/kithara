//! Filenames under `crates/*/src/` should be a single word.
//!
//! Rationale: short names compose well with the module path. `kithara-file::stream`
//! beats `kithara-file::stream_type::File`. Existing multi-word names lock into
//! the baseline; new files must conform unless explicitly exempted.

use anyhow::Result;

use super::{Check, Context};
use crate::{
    arch::config::AccessorSeverity,
    common::{
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files},
    },
};

pub(crate) const ID: &str = "single_word_filenames";

pub(crate) struct SingleWordFilenames;

impl Check for SingleWordFilenames {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.single_word_filenames;
        if cfg.severity == AccessorSeverity::Off {
            return Ok(Vec::new());
        }
        let exempt = compile_globs(&cfg.exempt_globs);
        let mut violations = Vec::new();

        for path in workspace_rs_files(ctx.workspace_root)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if cfg.exempt_filenames.iter().any(|n| n == file_name) {
                continue;
            }
            let words = word_count(file_name);
            if words <= cfg.max_words {
                continue;
            }
            let key = rel.to_string_lossy().replace('\\', "/");
            let stem = file_name.strip_suffix(".rs").unwrap_or(file_name);
            let suggestion = stem.split('_').next().unwrap_or(stem);
            let msg = format!(
                "filename `{file_name}` has {words} words (max {}); \
                 prefer a single-word name (e.g. `{suggestion}.rs`) — split the \
                 module if one word does not capture its content",
                cfg.max_words,
            );
            violations.push(emit(cfg.severity, key, msg));
        }
        Ok(violations)
    }
}

fn word_count(filename: &str) -> usize {
    let stem = filename.strip_suffix(".rs").unwrap_or(filename);
    stem.split('_').filter(|p| !p.is_empty()).count()
}

fn emit(sev: AccessorSeverity, key: String, message: String) -> Violation {
    match sev {
        AccessorSeverity::Deny => Violation::deny(ID, key, message),
        AccessorSeverity::Warn => Violation::warn(ID, key, message),
        AccessorSeverity::Off => unreachable!("off severity is short-circuited above"),
    }
}

#[cfg(test)]
mod tests {
    use super::word_count;

    #[test]
    fn single_word_counted_as_one() {
        assert_eq!(word_count("peer.rs"), 1);
        assert_eq!(word_count("source.rs"), 1);
    }

    #[test]
    fn underscored_pair_counted_as_two() {
        assert_eq!(word_count("stream_type.rs"), 2);
        assert_eq!(word_count("playlist_cache.rs"), 2);
    }

    #[test]
    fn three_words() {
        assert_eq!(word_count("hls_segment_loader.rs"), 3);
    }

    #[test]
    fn leading_underscore_does_not_count() {
        // `_internal.rs` is one word, not two
        assert_eq!(word_count("_internal.rs"), 1);
    }

    #[test]
    fn trailing_underscore_does_not_count() {
        assert_eq!(word_count("foo_.rs"), 1);
    }
}

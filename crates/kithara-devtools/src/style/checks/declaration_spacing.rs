use anyhow::Result;
use syn::{
    Block, File, ImplItem, Item, ItemImpl, ItemMod, ItemTrait, Stmt, TraitItem,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{fix::FixOutcome, violation::Violation, walker::relative_to};

pub(crate) mod consts {
    pub(crate) const ID: &str = "declaration_spacing";
}

/// One-line `const`, `static` or `type` declarations of one kind, set apart
/// by a blank line after every single one. A blank line between neighbours
/// that are otherwise alike marks no group, so the run is written solid. Runs
/// where some neighbours already touch are grouped on purpose and stay as they
/// are, as does anything with a doc comment, an attribute or a comment of its
/// own.
pub(crate) struct DeclarationSpacing;

impl Check for DeclarationSpacing {
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let mut outcome = FixOutcome::default();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let (Some(src), Ok(file)) = (ctx.scan.source(path), ctx.scan.parse_file(path)) else {
                continue;
            };
            let runs = spaced_runs(&src, &file);
            if runs.is_empty() {
                continue;
            }
            ctx.scan.write(path, close_up(&src, &runs))?;
            outcome.writes += 1;
        }
        Ok(outcome)
    }

    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let mut violations = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let (Some(src), Ok(file)) = (ctx.scan.source(path), ctx.scan.parse_file(path)) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            for run in spaced_runs(&src, &file) {
                violations.push(Violation::deny(
                    consts::ID,
                    format!("{rel}:{}", run.first_line),
                    format!(
                        "{} one-line `{}` declarations each sit apart behind a blank line; \
                         write them solid",
                        run.len,
                        run.kind.keyword()
                    ),
                ));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    /// Blank lines in a test read the same as anywhere else.
    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Const,
    Static,
    Type,
}

impl Kind {
    const fn keyword(self) -> &'static str {
        match self {
            Self::Const => "const",
            Self::Static => "static",
            Self::Type => "type",
        }
    }
}

/// A declaration in an item list or block, with 1-based lines. `kind` is set
/// only for a one-line `const`, `static` or `type` that carries no attribute,
/// doc comments included.
#[derive(Clone, Copy)]
struct Entry {
    end: usize,
    kind: Option<Kind>,
    start: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct Run {
    /// Lines between the declarations, every one of them blank.
    blank_lines: Vec<usize>,
    first_line: usize,
    kind: Kind,
    len: usize,
}

fn spaced_runs(src: &str, file: &File) -> Vec<Run> {
    let lines: Vec<&str> = src.lines().collect();
    let mut sequences = Sequences::default();
    sequences.visit_file(file);
    sequences
        .lists
        .iter()
        .flat_map(|entries| runs_in(&lines, entries))
        .collect()
}

fn runs_in(lines: &[&str], entries: &[Entry]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut chain: Vec<Entry> = Vec::new();
    for entry in entries {
        let joins = chain.last().is_some_and(|last| {
            entry.kind.is_some()
                && last.kind == entry.kind
                && gap(lines, last, entry).all(|line| line.trim().is_empty())
        });
        if !joins {
            runs.extend(spaced(&chain));
            chain.clear();
        }
        if entry.kind.is_some() {
            chain.push(*entry);
        }
    }
    runs.extend(spaced(&chain));
    runs
}

/// The run a chain forms when a blank line parts every pair in it.
fn spaced(chain: &[Entry]) -> Option<Run> {
    let (first, kind) = (chain.first()?, chain.first()?.kind?);
    let every_pair_parted =
        chain.len() >= 2 && chain.windows(2).all(|pair| pair[0].end + 1 < pair[1].start);
    every_pair_parted.then(|| Run {
        blank_lines: chain
            .windows(2)
            .flat_map(|pair| pair[0].end + 1..pair[1].start)
            .collect(),
        first_line: first.start,
        kind,
        len: chain.len(),
    })
}

/// Source lines strictly between two entries.
fn gap<'a>(lines: &'a [&'a str], before: &Entry, after: &Entry) -> impl Iterator<Item = &'a str> {
    lines
        .get(before.end..after.start.saturating_sub(1))
        .unwrap_or_default()
        .iter()
        .copied()
}

fn close_up(src: &str, runs: &[Run]) -> String {
    let dropped: std::collections::HashSet<usize> = runs
        .iter()
        .flat_map(|run| run.blank_lines.iter().copied())
        .collect();
    src.split_inclusive('\n')
        .enumerate()
        .filter(|(index, _)| !dropped.contains(&(index + 1)))
        .map(|(_, line)| line)
        .collect()
}

/// Every item list and block in a file, as the declarations it holds.
#[derive(Default)]
struct Sequences {
    lists: Vec<Vec<Entry>>,
}

impl<'ast> Visit<'ast> for Sequences {
    fn visit_block(&mut self, block: &'ast Block) {
        self.lists.push(
            block
                .stmts
                .iter()
                .map(|stmt| match stmt {
                    Stmt::Item(item) => item_entry(item),
                    other => entry(other, None, false),
                })
                .collect(),
        );
        visit::visit_block(self, block);
    }

    fn visit_file(&mut self, file: &'ast File) {
        self.lists.push(file.items.iter().map(item_entry).collect());
        visit::visit_file(self, file);
    }

    fn visit_item_impl(&mut self, block: &'ast ItemImpl) {
        self.lists.push(
            block
                .items
                .iter()
                .map(|item| match item {
                    ImplItem::Const(c) => entry(item, Some(Kind::Const), c.attrs.is_empty()),
                    ImplItem::Type(t) => entry(item, Some(Kind::Type), t.attrs.is_empty()),
                    other => entry(other, None, false),
                })
                .collect(),
        );
        visit::visit_item_impl(self, block);
    }

    fn visit_item_mod(&mut self, module: &'ast ItemMod) {
        if let Some((_, items)) = &module.content {
            self.lists.push(items.iter().map(item_entry).collect());
        }
        visit::visit_item_mod(self, module);
    }

    fn visit_item_trait(&mut self, block: &'ast ItemTrait) {
        self.lists.push(
            block
                .items
                .iter()
                .map(|item| match item {
                    TraitItem::Const(c) => entry(item, Some(Kind::Const), c.attrs.is_empty()),
                    TraitItem::Type(t) => entry(item, Some(Kind::Type), t.attrs.is_empty()),
                    other => entry(other, None, false),
                })
                .collect(),
        );
        visit::visit_item_trait(self, block);
    }
}

fn item_entry(item: &Item) -> Entry {
    match item {
        Item::Const(c) => entry(item, Some(Kind::Const), c.attrs.is_empty()),
        Item::Static(s) => entry(item, Some(Kind::Static), s.attrs.is_empty()),
        Item::Type(t) => entry(item, Some(Kind::Type), t.attrs.is_empty()),
        other => entry(other, None, false),
    }
}

fn entry(node: &impl Spanned, kind: Option<Kind>, bare: bool) -> Entry {
    let span = node.span();
    let (start, end) = (span.start().line, span.end().line);
    Entry {
        end,
        kind: kind.filter(|_| bare && start == end),
        start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(src: &str) -> Vec<Run> {
        spaced_runs(src, &syn::parse_file(src).expect("fixture parses"))
    }

    fn fixed(src: &str) -> String {
        close_up(src, &runs(src))
    }

    #[test]
    fn a_run_spaced_after_every_declaration_is_written_solid() {
        let src = "fn f() {\n    const A: u8 = 1;\n\n    const B: u8 = 2;\n\n\n    const C: u8 = 3;\n\n    let _ = A;\n}\n";

        assert_eq!(
            fixed(src),
            "fn f() {\n    const A: u8 = 1;\n    const B: u8 = 2;\n    const C: u8 = 3;\n\n    let _ = A;\n}\n"
        );
        assert_eq!(runs(src)[0].first_line, 2);
        assert_eq!(runs(src)[0].len, 3);
    }

    #[test]
    fn type_aliases_statics_and_associated_items_close_up_too() {
        let src = "pub type A = u8;\n\npub type B = u16;\n\nstatic S: u8 = 0;\n\nstatic T: u8 = 0;\n\nimpl X {\n    const P: u8 = 0;\n\n    const Q: u8 = 0;\n}\n";

        assert_eq!(
            fixed(src),
            "pub type A = u8;\npub type B = u16;\n\nstatic S: u8 = 0;\nstatic T: u8 = 0;\n\nimpl X {\n    const P: u8 = 0;\n    const Q: u8 = 0;\n}\n"
        );
    }

    #[test]
    fn groups_that_already_touch_keep_the_blank_line_between_them() {
        let src = "const A1: u8 = 0;\nconst A2: u8 = 0;\n\nconst B1: u8 = 0;\nconst B2: u8 = 0;\n";

        assert!(runs(src).is_empty());
    }

    #[test]
    fn a_documented_attributed_or_commented_declaration_breaks_the_run() {
        for src in [
            "const A: u8 = 0;\n\n/// Why B.\nconst B: u8 = 0;\n",
            "const A: u8 = 0;\n\n#[cfg(test)]\nconst B: u8 = 0;\n",
            "const A: u8 = 0;\n\n// why B\nconst B: u8 = 0;\n",
            "const A: u8 = 0;\n\n// floating\n\nconst B: u8 = 0;\n",
        ] {
            assert!(runs(src).is_empty(), "{src}");
        }
    }

    #[test]
    fn different_kinds_and_multi_line_declarations_are_left_alone() {
        for src in [
            "const A: u8 = 0;\n\nstatic B: u8 = 0;\n",
            "const A: [u8; 2] = [\n    0, 1,\n];\n\nconst B: u8 = 0;\n",
            "const A: u8 = 0;\n\nfn f() {}\n\nconst B: u8 = 0;\n",
        ] {
            assert!(runs(src).is_empty(), "{src}");
        }
    }
}

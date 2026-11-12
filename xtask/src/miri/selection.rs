use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::{Ctx, common::walker::walk_rs_files};
use kithara_test_args::TestArgs;
use syn::visit::{self, Visit};

pub(super) fn check(ctx: &Ctx, packages: &[String]) -> Result<()> {
    let mut marked = BTreeMap::new();
    for package in ctx.metadata()?.workspace_packages() {
        let root = package
            .manifest_path
            .parent()
            .context("package has no directory")?;
        let mut files = BTreeSet::new();
        for target in &package.targets {
            let directory = target
                .src_path
                .parent()
                .context("target has no directory")?;
            files.extend(walk_rs_files(directory.as_std_path())?);
        }
        let mut names = Vec::new();
        for file in files {
            let source = fs::read_to_string(&file)?;
            let syntax = syn::parse_file(&source)
                .with_context(|| format!("parse {} for Miri selection", file.display()))?;
            for name in marked_tests(&syntax)
                .with_context(|| format!("Miri attributes in {}", file.display()))?
            {
                names.push(format!(
                    "{}::{name}",
                    file.strip_prefix(root.as_std_path())?.display()
                ));
            }
        }
        marked.insert(package.name.to_string(), names);
    }
    validate(packages, &marked)
}

fn validate(packages: &[String], marked: &BTreeMap<String, Vec<String>>) -> Result<()> {
    let listed: BTreeSet<_> = packages.iter().map(String::as_str).collect();
    let mut failures = Vec::new();
    for package in packages {
        if marked.get(package).is_none_or(Vec::is_empty) {
            failures.push(format!(
                "deep-miri package `{package}` has no Miri-marked tests"
            ));
        }
    }
    for (package, tests) in marked {
        if !listed.contains(package.as_str()) && !tests.is_empty() {
            failures.push(format!(
                "Miri-marked tests in unlisted package `{package}`: {}",
                tests.join(", ")
            ));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn marked_tests(file: &syn::File) -> Result<Vec<String>> {
    let mut visitor = Marks::default();
    visitor.visit_file(file);
    ensure!(visitor.errors.is_empty(), "{}", visitor.errors.join("\n"));
    Ok(visitor.tests)
}

#[derive(Default)]
struct Marks {
    errors: Vec<String>,
    tests: Vec<String>,
}

impl<'ast> Visit<'ast> for Marks {
    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        for attribute in &function.attrs {
            let path = attribute.path();
            if path.segments.len() != 2
                || path.segments[0].ident != "kithara"
                || path.segments[1].ident != "test"
            {
                continue;
            }
            let args = match &attribute.meta {
                syn::Meta::Path(_) => Ok(TestArgs::default()),
                _ => attribute.parse_args::<TestArgs>(),
            };
            match args {
                Ok(args) if args.is_miri => self.tests.push(function.sig.ident.to_string()),
                Ok(_) => {}
                Err(error) => self.errors.push(format!("{}: {error}", function.sig.ident)),
            }
        }
        visit::visit_item_fn(self, function);
    }
}

#[cfg(test)]
mod tests {
    use super::{marked_tests, validate};

    #[test]
    fn marked_tests_require_a_listed_package_and_every_package_requires_marks() {
        let marked = [
            ("queue".to_owned(), vec!["load_play_seek_stop".to_owned()]),
            ("ring".to_owned(), Vec::new()),
        ]
        .into();
        let error = validate(&["ring".to_owned()], &marked).expect_err("invalid selection");
        let message = error.to_string();
        assert!(message.contains("ring") && message.contains("has no Miri-marked tests"));
        assert!(message.contains("queue") && message.contains("load_play_seek_stop"));
        assert!(validate(&["queue".to_owned()], &marked).is_ok());
    }

    #[test]
    fn selection_uses_the_macro_parser_in_nested_modules() -> syn::Result<()> {
        let syntax = syn::parse_file(
            r#"
            mod nested {
                #[kithara::test(miri, tokio, timeout(std::time::Duration::from_secs(3)))]
                async fn selected() {}
                #[kithara::test] fn ordinary() {}
                #[kithara::test(tokio)] async fn native() {}
            }
        "#,
        )?;
        assert_eq!(
            marked_tests(&syntax).expect("valid attributes"),
            ["selected"]
        );
        let invalid = syn::parse_file("#[kithara::test(miri, typo)] fn broken() {}")?;
        assert!(marked_tests(&invalid).is_err());
        Ok(())
    }
}

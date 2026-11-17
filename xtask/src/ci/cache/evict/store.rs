use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::super::require_success;
use crate::consts;

/// The store as the evictor reaches it: `rc` under the administrator's alias.
pub(super) struct Store {
    program: PathBuf,
}

/// One object a listing returned, its key relative to the bucket.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub(super) key: String,
    pub(super) size: u64,
    pub(super) written_s: u64,
}

/// What one listing returned: the objects, and the prefixes a listing that
/// does not recurse stops at.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Page {
    pub(super) objects: Vec<Listed>,
    pub(super) prefixes: Vec<String>,
}

/// What `rc --json object list` prints. It pages through the whole listing
/// before printing, so an unfinished one is a contract it broke.
#[derive(Deserialize)]
struct Listing {
    items: Vec<Item>,
    truncated: bool,
}

#[derive(Deserialize)]
struct Item {
    key: String,
    is_dir: bool,
    size_bytes: Option<u64>,
    last_modified: Option<String>,
}

impl Store {
    const ALIAS: &str = "ci";
    /// The exit status `rc` gives an object that does not exist.
    const NOT_FOUND: i32 = 5;

    pub(super) fn connect(program: &Path, user: &str, password: &str) -> Result<Self> {
        let status = Command::new(program)
            .args([
                "alias",
                "set",
                "--",
                Self::ALIAS,
                consts::CACHE_STORE_URL,
                user,
                password,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("start the store client")?;
        // The arguments carry the administrator's password.
        ensure!(
            status.success(),
            "configuring the store client failed: {status}"
        );
        Ok(Self {
            program: program.to_owned(),
        })
    }

    pub(super) fn list(&self, bucket: &str, prefix: &str, recursive: bool) -> Result<Page> {
        let remote = Self::remote(bucket, prefix);
        let mut arguments = vec!["--json", "object", "list"];
        if recursive {
            arguments.push("--recursive");
        }
        arguments.push(&remote);
        let output = self.output(&arguments)?;
        require_success(&output, "list the bucket")?;
        let listing: Listing =
            serde_json::from_slice(&output.stdout).context("read the bucket listing")?;
        ensure!(!listing.truncated, "the listing of {remote} is unfinished");
        let mut page = Page::default();
        for item in listing.items {
            if item.is_dir {
                page.prefixes.push(item.key);
                continue;
            }
            let size = item
                .size_bytes
                .with_context(|| format!("{} is listed without a size", item.key))?;
            let written = item
                .last_modified
                .with_context(|| format!("{} is listed without a date", item.key))?;
            ensure!(
                written.as_bytes().get(10) == Some(&b'T')
                    && (written.ends_with('Z') || written.ends_with("+00:00")),
                "{} is listed as written at {written}",
                item.key
            );
            let written_s = u64::try_from(
                time::OffsetDateTime::parse(
                    &written,
                    &time::format_description::well_known::Rfc3339,
                )
                .with_context(|| format!("{} is listed as written at {written}", item.key))?
                .unix_timestamp(),
            )
            .with_context(|| format!("{} was written before 1970", item.key))?;
            page.objects.push(Listed {
                key: item.key,
                size,
                written_s,
            });
        }
        Ok(page)
    }

    pub(super) fn remove(&self, bucket: &str, objects: &[String]) -> Result<()> {
        let mut command = Command::new(&self.program);
        command.args(["object", "remove", "--force"]);
        command.args(objects.iter().map(|object| Self::remote(bucket, object)));
        let output = command.output().context("start the store client")?;
        require_success(&output, "remove entries")
    }

    pub(super) fn put(&self, bucket: &str, object: &str, body: &[u8]) -> Result<()> {
        let mut child = Command::new(&self.program)
            .arg("pipe")
            .arg(Self::remote(bucket, object))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("start the store client")?;
        child
            .stdin
            .take()
            .context("the store client takes no input")?
            .write_all(body)
            .context("hand the object to the store client")?;
        let output = child
            .wait_with_output()
            .context("wait for the store client")?;
        require_success(&output, "write an object")
    }

    /// Only an object that is not there reads as absent; a read the store
    /// refused or never answered is a failure.
    pub(super) fn get(&self, bucket: &str, object: &str) -> Result<Option<Vec<u8>>> {
        let output = self.output(&["object", "show", &Self::remote(bucket, object)])?;
        if output.status.code() == Some(Self::NOT_FOUND) {
            return Ok(None);
        }
        require_success(&output, "read an object")?;
        Ok(Some(output.stdout))
    }

    fn output(&self, arguments: &[&str]) -> Result<Output> {
        Command::new(&self.program)
            .args(arguments)
            .output()
            .context("start the store client")
    }

    fn remote(bucket: &str, object: &str) -> String {
        format!("{}/{bucket}/{object}", Self::ALIAS)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::install_script;

    /// Speaks `rc` the way the store relies on, and records each call.
    fn store(directory: &Path, cases: &str) -> Store {
        let program = directory.join("rc");
        install_script(
            &program,
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
case "$*" in
"alias set -- ci {store} user password") ;;
{cases}
*) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
                log = directory.join("log").display(),
                store = consts::CACHE_STORE_URL,
            ),
        );
        Store::connect(&program, "user", "password").expect("the stand-in store connects")
    }

    #[test]
    fn a_listing_parts_objects_from_prefixes_and_reads_their_seconds() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(
            directory.path(),
            r#""--json object list ci/kithara-review/sccache/") printf '%s' '{"items":[{"key":"sccache/a/","is_dir":true},{"key":"sccache/.sccache_check","size_bytes":13,"last_modified":"1970-01-01T00:01:40.5Z","is_dir":false}],"truncated":false}' ;;
"--json object list --recursive ci/kithara-review/sccache/a/") printf '%s' '{"items":[{"key":"sccache/a/b/c/d","size_bytes":300,"last_modified":"1970-01-01T00:05:00Z","is_dir":false}],"truncated":false}' ;;"#,
        );

        let top = store.list("kithara-review", "sccache/", false).unwrap();
        let child = store.list("kithara-review", "sccache/a/", true).unwrap();

        assert_eq!(
            top,
            Page {
                objects: vec![Listed {
                    key: "sccache/.sccache_check".to_owned(),
                    size: 13,
                    written_s: 100,
                }],
                prefixes: vec!["sccache/a/".to_owned()],
            }
        );
        assert_eq!(child.objects.len(), 1);
        assert_eq!(child.objects[0].written_s, 300);
    }

    /// A listing that missed objects would undercount the bucket, and an
    /// object without a size or a date cannot be weighed or aged.
    #[test]
    fn a_listing_that_does_not_account_for_every_object_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(
            directory.path(),
            r#""--json object list ci/kithara-review/truncated/") printf '%s' '{"items":[],"truncated":true}' ;;
"--json object list ci/kithara-review/sizeless/") printf '%s' '{"items":[{"key":"sizeless/a","last_modified":"1970-01-01T00:05:00Z","is_dir":false}],"truncated":false}' ;;
"--json object list ci/kithara-review/undated/") printf '%s' '{"items":[{"key":"undated/a","size_bytes":1,"is_dir":false}],"truncated":false}' ;;"#,
        );

        for prefix in ["truncated/", "sizeless/", "undated/"] {
            assert!(
                store.list("kithara-review", prefix, false).is_err(),
                "{prefix}"
            );
        }
    }

    #[test]
    fn only_an_object_that_is_not_there_reads_as_absent() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(
            directory.path(),
            r#""object show ci/ci-cache-recency/absent") echo "Not found" >&2; exit 5 ;;
"object show ci/ci-cache-recency/denied") echo "Access denied" >&2; exit 4 ;;
"object show ci/ci-cache-recency/present") printf 'record' ;;"#,
        );

        assert_eq!(store.get("ci-cache-recency", "absent").unwrap(), None);
        assert!(store.get("ci-cache-recency", "denied").is_err());
        assert_eq!(
            store.get("ci-cache-recency", "present").unwrap().as_deref(),
            Some(&b"record"[..])
        );
    }

    #[test]
    fn a_write_carries_its_body_and_a_removal_names_every_object() {
        let directory = tempfile::tempdir().unwrap();
        let body = directory.path().join("body");
        let store = store(
            directory.path(),
            &format!(
                r#""pipe ci/ci-cache-recency/kithara-review") cat > '{body}' ;;
"object remove --force ci/kithara-review/a ci/kithara-review/b") ;;"#,
                body = body.display()
            ),
        );

        store
            .put("ci-cache-recency", "kithara-review", b"record")
            .unwrap();
        store
            .remove("kithara-review", &["a".to_owned(), "b".to_owned()])
            .unwrap();

        assert_eq!(std::fs::read(body).unwrap(), b"record");
    }
}

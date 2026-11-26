use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{Github, client::string_field};
use crate::consts::PLAN_RETENTION_REF;

/// Where a plan view lives in the retention tree, named by its digest.
pub(in crate::ci::bridge) fn view_path(sha256: &str) -> String {
    format!(".ci-artifacts/plan-control/v3/{sha256}.json")
}

impl Github {
    /// The numeric id of the configured repository.
    pub(in crate::ci::bridge) fn repository_id(&self) -> Result<u64> {
        self.request(
            &Method::GET,
            &format!("/repos/{}", self.config.github_repo),
            None,
        )?["id"]
            .as_u64()
            .context("GitHub repository has no numeric id")
    }

    /// Append a plan view to the retention branch and return the commit that
    /// holds it, once the file at that commit reads back as `bytes`.
    ///
    /// A tip that already holds the same blob at the same path is that commit,
    /// so a crash after the branch moved recovers it instead of appending a
    /// twin. The branch only fast-forwards: a competing writer fails the append.
    pub(in crate::ci::bridge) fn retain_plan_view(
        &self,
        bytes: &[u8],
        sha256: &str,
    ) -> Result<String> {
        let repo = &self.config.github_repo;
        let path = view_path(sha256);
        let post = |endpoint: &str, body: &Value| -> Result<String> {
            string_field(
                &self.request(
                    &Method::POST,
                    &format!("/repos/{repo}/{endpoint}"),
                    Some(body),
                )?,
                &["sha"],
                "GitHub object",
            )
        };
        let blob = post(
            "git/blobs",
            &json!({"content": STANDARD.encode(bytes), "encoding": "base64"}),
        )?;
        let refs = self.request(
            &Method::GET,
            &format!("/repos/{repo}/git/matching-refs/{PLAN_RETENTION_REF}"),
            None,
        )?;
        let tip = refs
            .as_array()
            .context("GitHub matching refs are not a list")?
            .iter()
            .find(|reference| reference["ref"] == format!("refs/{PLAN_RETENTION_REF}"))
            .map(|reference| string_field(reference, &["object", "sha"], "retention tip"))
            .transpose()?;
        let entry = json!([{"path": path, "mode": "100644", "type": "blob", "sha": blob}]);
        let message = format!("plan-control: retain {sha256}");
        let commit = if let Some(tip) = tip {
            let parent = self.request(
                &Method::GET,
                &format!("/repos/{repo}/git/commits/{tip}"),
                None,
            )?;
            let base = string_field(&parent, &["tree", "sha"], "retention tree")?;
            let tree = post("git/trees", &json!({"base_tree": base, "tree": entry}))?;
            if tree == base {
                tip
            } else {
                let commit = post(
                    "git/commits",
                    &json!({"message": message, "tree": tree, "parents": [tip]}),
                )?;
                self.request(
                    &Method::PATCH,
                    &format!("/repos/{repo}/git/refs/{PLAN_RETENTION_REF}"),
                    Some(&json!({"sha": commit, "force": false})),
                )?;
                commit
            }
        } else {
            let tree = post("git/trees", &json!({"tree": entry}))?;
            let commit = post(
                "git/commits",
                &json!({"message": message, "tree": tree, "parents": []}),
            )?;
            self.request(
                &Method::POST,
                &format!("/repos/{repo}/git/refs"),
                Some(&json!({"ref": format!("refs/{PLAN_RETENTION_REF}"), "sha": commit})),
            )?;
            commit
        };
        let file = self.request(
            &Method::GET,
            &format!("/repos/{repo}/contents/{path}?ref={commit}"),
            None,
        )?;
        read_back(&file, bytes, sha256, &blob)?;
        Ok(commit)
    }
}

/// Require the retained file to be exactly the sealed bytes: a regular file
/// of their size, whose blob and SHA-256 are theirs.
fn read_back(file: &Value, bytes: &[u8], sha256: &str, blob: &str) -> Result<()> {
    let content = file["content"]
        .as_str()
        .context("retained plan view has no content")?;
    let decoded = STANDARD
        .decode(content.replace('\n', ""))
        .context("retained plan view is not base64")?;
    ensure!(
        file["type"] == "file"
            && file["encoding"] == "base64"
            && file["size"]
                .as_u64()
                .and_then(|size| usize::try_from(size).ok())
                == Some(bytes.len())
            && file["sha"] == blob
            && hex::encode(Sha256::digest(&decoded)) == sha256
            && decoded == bytes,
        "retained plan view does not read back as the sealed bytes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_retained_file_reads_back() {
        let bytes = br#"{"version":3}"#;
        let sha256 = hex::encode(Sha256::digest(bytes));
        let encoded = STANDARD.encode(bytes);
        let file = json!({"type": "file", "encoding": "base64", "size": bytes.len(),
            "sha": "b".repeat(40), "content": format!("{}\n{}", &encoded[..8], &encoded[8..])});
        read_back(&file, bytes, &sha256, &"b".repeat(40)).unwrap();

        for (field, value) in [
            ("type", json!("symlink")),
            ("size", json!(bytes.len() + 1)),
            ("sha", json!("c".repeat(40))),
            ("content", json!(STANDARD.encode(br#"{"version":4}"#))),
            ("encoding", json!("none")),
        ] {
            let mut changed = file.clone();
            changed[field] = value;
            assert!(
                read_back(&changed, bytes, &sha256, &"b".repeat(40)).is_err(),
                "{field}"
            );
        }
        assert!(read_back(&file, bytes, &"0".repeat(64), &"b".repeat(40)).is_err());
    }
}

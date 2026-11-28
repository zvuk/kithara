//! The registration control jobs run on.
//!
//! A control job reads and writes the verdict journal and calls APIs; it builds
//! nothing. The build registrations share one `concurrent` limit, so a control
//! job queued behind them waited for a build slot it had no use for. This
//! registration runs in a runner process of its own, one job at a time.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, de::DeserializeOwned};
use tracing::info;

use super::runners::{read_token, write_secure};
use crate::consts;

/// Where a registration is issued, and the credentials that may issue one.
pub(super) struct Gitlab<'a> {
    /// The REST root, `<origin>/api/v4`.
    pub(super) api: &'a str,
    pub(super) project: Option<&'a str>,
    pub(super) token: Option<&'a str>,
}

#[derive(Deserialize)]
struct AccessToken {
    scopes: Vec<String>,
}

#[derive(Deserialize)]
struct Issued {
    token: String,
}

/// Leaves a token GitLab accepts in `root`, issuing a registration only when
/// there is no token file.
///
/// A file whose token GitLab rejects stops provisioning. Issuing another would
/// leave the rejected registration behind and hide why it stopped working.
pub(super) fn enrol(root: &Path, gitlab: &Gitlab<'_>) -> Result<()> {
    let path = root.join(format!("runner-{}.token", consts::MAC_CONTROL_RUNNER));
    let client = Client::builder()
        .no_proxy()
        .timeout(consts::GITLAB_API_TIMEOUT)
        .build()
        .context("building the GitLab client")?;
    if path.try_exists()? {
        let token = read_token(root, consts::MAC_CONTROL_RUNNER)?;
        let response = client
            .post(format!("{}/runners/verify", gitlab.api))
            .form(&[("token", token.as_str())])
            .send()
            .context("verifying the control runner token")?;
        return match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::FORBIDDEN => bail!(
                "GitLab rejects the control runner token in {}: its registration is gone. \
                 Remove the file and provision again to issue a new one",
                path.display()
            ),
            status => bail!("verifying the control runner token failed with HTTP {status}"),
        };
    }

    let token = gitlab
        .token
        .context("GITLAB_TOKEN is not set: provisioning issues the control runner with it")?;
    let project = gitlab
        .project
        .context("CI_PROJECT_ID is not set: the control runner is issued to this project")?;
    let mut headers = HeaderMap::new();
    let mut credential =
        HeaderValue::from_str(token).context("GITLAB_TOKEN is not a usable header value")?;
    credential.set_sensitive(true);
    headers.insert("PRIVATE-TOKEN", credential);

    let granted: AccessToken = json(
        client
            .get(format!("{}/personal_access_tokens/self", gitlab.api))
            .headers(headers.clone())
            .send()
            .context("reading the scopes of GITLAB_TOKEN")?,
        "reading the scopes of GITLAB_TOKEN",
    )?;
    ensure!(
        granted
            .scopes
            .iter()
            .any(|scope| scope == consts::CREATE_RUNNER_SCOPE),
        "GITLAB_TOKEN lacks {}: grant it or issue the control runner token",
        consts::CREATE_RUNNER_SCOPE
    );

    let issued: Issued = json(
        client
            .post(format!("{}/user/runners", gitlab.api))
            .headers(headers)
            .form(&[
                ("runner_type", "project_type"),
                ("project_id", project),
                ("description", consts::MAC_CONTROL_DESCRIPTION),
                ("tag_list", consts::MAC_CONTROL_TAG),
                ("run_untagged", "false"),
                ("locked", "true"),
                ("access_level", "not_protected"),
            ])
            .send()
            .context("issuing the control runner")?,
        "issuing the control runner",
    )?;
    ensure!(
        issued.token.starts_with("glrt-") && !issued.token.chars().any(char::is_whitespace),
        "GitLab issued the control runner an unexpected token"
    );
    write_secure(&path, &issued.token)?;
    info!(tag = consts::MAC_CONTROL_TAG, "control runner issued");
    Ok(())
}

fn json<T: DeserializeOwned>(response: Response, action: &str) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        bail!("{action} failed with HTTP {status}");
    }
    response
        .json()
        .with_context(|| format!("{action}: unreadable response"))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    /// One request the fake GitLab saw: method, path and form body.
    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

    /// A GitLab that answers each path from `routes` and records every request.
    fn gitlab(routes: Vec<(&'static str, u16, &'static str)>) -> (String, Seen) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind the fake GitLab");
        let api = format!("http://{}/api/v4", server.server_addr());
        let seen = Seen::default();
        let record = Arc::clone(&seen);
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                request
                    .as_reader()
                    .read_to_string(&mut body)
                    .expect("read the request body");
                let path = request.url().trim_start_matches("/api/v4").to_owned();
                let (status, answer) = routes
                    .iter()
                    .find(|(route, ..)| *route == path)
                    .map_or((404, "{}"), |(_, status, answer)| (*status, *answer));
                record.lock().expect("record lock").push((
                    request.method().to_string(),
                    path,
                    body,
                ));
                request
                    .respond(tiny_http::Response::from_string(answer).with_status_code(status))
                    .expect("answer the request");
            }
        });
        (api, seen)
    }

    fn paths(seen: &Seen) -> Vec<String> {
        seen.lock()
            .expect("record lock")
            .iter()
            .map(|(method, path, _)| format!("{method} {path}"))
            .collect()
    }

    fn token_file(root: &Path) -> std::path::PathBuf {
        root.join(format!("runner-{}.token", consts::MAC_CONTROL_RUNNER))
    }

    #[test]
    fn a_token_without_create_runner_issues_nothing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (api, seen) = gitlab(vec![(
            "/personal_access_tokens/self",
            200,
            r#"{"scopes":["api","read_repository"]}"#,
        )]);

        let error = enrol(
            directory.path(),
            &Gitlab {
                api: &api,
                project: Some("42"),
                token: Some("glpat-fixture"),
            },
        )
        .expect_err("a token that cannot create runners issues none");

        assert!(
            format!("{error:#}").contains(
                "GITLAB_TOKEN lacks create_runner: grant it or issue the control runner token"
            ),
            "{error:#}"
        );
        assert_eq!(paths(&seen), ["GET /personal_access_tokens/self"]);
        assert!(!token_file(directory.path()).exists());
    }

    #[test]
    fn a_rejected_token_stops_provisioning_instead_of_issuing_another() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_secure(&token_file(directory.path()), "glrt-removed").expect("write the token");
        let (api, seen) = gitlab(vec![("/runners/verify", 403, "{}")]);

        let error = enrol(
            directory.path(),
            &Gitlab {
                api: &api,
                project: Some("42"),
                token: Some("glpat-fixture"),
            },
        )
        .expect_err("a rejected token is an error");

        assert!(
            format!("{error:#}").contains("rejects the control runner token"),
            "{error:#}"
        );
        assert_eq!(paths(&seen), ["POST /runners/verify"]);
        assert_eq!(
            fs::read_to_string(token_file(directory.path())).expect("read the token"),
            "glrt-removed"
        );
    }

    #[test]
    fn a_missing_token_is_issued_once_under_the_control_tag_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (api, seen) = gitlab(vec![
            (
                "/personal_access_tokens/self",
                200,
                r#"{"scopes":["api","create_runner"]}"#,
            ),
            ("/user/runners", 201, r#"{"id":7,"token":"glrt-control"}"#),
            ("/runners/verify", 200, "{}"),
        ]);
        let gitlab = Gitlab {
            api: &api,
            project: Some("42"),
            token: Some("glpat-fixture"),
        };

        enrol(directory.path(), &gitlab).expect("the registration is issued");
        enrol(directory.path(), &gitlab).expect("the issued token is accepted");

        assert_eq!(
            read_token(directory.path(), consts::MAC_CONTROL_RUNNER)
                .expect("a 0600 glrt token file"),
            "glrt-control"
        );
        assert_eq!(
            paths(&seen),
            [
                "GET /personal_access_tokens/self",
                "POST /user/runners",
                "POST /runners/verify",
            ]
        );
        let form = seen.lock().expect("record lock")[1].2.clone();
        let fields: Vec<(String, String)> = form
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        for expected in [
            ("runner_type", "project_type"),
            ("project_id", "42"),
            ("tag_list", consts::MAC_CONTROL_TAG),
            ("run_untagged", "false"),
        ] {
            assert!(
                fields
                    .iter()
                    .any(|(key, value)| (key.as_str(), value.as_str()) == expected),
                "{expected:?} missing from {form}"
            );
        }
    }
}

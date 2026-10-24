use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{
    Method, Url,
    blocking::{Client, RequestBuilder},
};
use serde_json::Value;

pub(super) enum Payload<'a> {
    Json(&'a Value),
    Form(&'a [(String, String)]),
}

pub(super) struct Api {
    client: Client,
}

impl Api {
    pub(super) fn new() -> Result<Self> {
        let builder = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("kithara-git-bridge/1")
            .redirect(reqwest::redirect::Policy::none());
        Ok(Self {
            client: builder.build().context("building bridge HTTP client")?,
        })
    }

    pub(super) fn request(
        &self,
        method: &Method,
        url: &Url,
        authorization: (&str, &str),
        payload: Option<&Payload<'_>>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method.clone(), url.clone())
            .header(authorization.0, authorization.1);
        request = match payload {
            Some(Payload::Json(body)) => request.json(body),
            Some(Payload::Form(form)) => request.form(form),
            None => request,
        };
        response_json(request, method, url)
    }
}

fn response_json(request: RequestBuilder, method: &Method, url: &Url) -> Result<Value> {
    let response = request
        .send()
        .with_context(|| format!("{method} {url} failed"))?;
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("reading {method} {url} response"))?;
    if !status.is_success() {
        let detail = body.chars().take(4_096).collect::<String>();
        bail!("{method} {url} failed with HTTP {status}: {detail}");
    }
    if body.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&body).with_context(|| format!("parsing {method} {url} response"))
    }
}

pub(super) fn paginated(
    mut fetch: impl FnMut(usize) -> Result<Value>,
    field: Option<&str>,
) -> Result<Vec<Value>> {
    const PAGE_SIZE: usize = 100;
    let mut result = Vec::new();
    let mut page = 1;
    loop {
        let response = fetch(page)?;
        let values = field.map_or(&response, |field| &response[field]);
        let values = values.as_array().context("API page was not an array")?;
        result.extend(values.iter().cloned());
        if values.len() < PAGE_SIZE {
            return Ok(result);
        }
        page += 1;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn all_pages_are_collected_for_jobs_bridges_runs_and_commit_statuses() {
        for field in [None, Some("jobs"), Some("workflow_runs"), Some("statuses")] {
            let mut requested = Vec::new();
            let values = paginated(
                |page| {
                    requested.push(page);
                    let count = if page < 3 { 100 } else { 1 };
                    let entries = (0..count)
                        .map(|index| json!({"id": (page - 1) * 100 + index}))
                        .collect::<Vec<_>>();
                    Ok(field.map_or_else(|| json!(entries), |field| json!({(field): entries})))
                },
                field,
            )
            .unwrap();
            assert_eq!(requested, [1, 2, 3]);
            assert_eq!(values.len(), 201);
            assert_eq!(values[200]["id"], 200);
        }
    }

    #[test]
    fn a_later_page_error_is_not_an_incomplete_success() {
        let result = paginated(
            |page| {
                if page == 1 {
                    Ok(json!(vec![Value::Null; 100]))
                } else {
                    bail!("second page unavailable")
                }
            },
            None,
        );
        assert!(result.is_err());
    }
}

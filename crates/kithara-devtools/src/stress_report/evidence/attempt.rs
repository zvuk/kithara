use std::collections::BTreeMap;

use crate::junit::CaseTiming;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttemptOutcome {
    Failed,
    Passed,
    Unattributed,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct AttemptKey {
    pub(super) name: String,
    pub(super) suite: String,
    pub(super) iteration: usize,
}

#[derive(Debug)]
pub(super) struct AttemptMetadata {
    pub(super) key: AttemptKey,
    pub(super) run_id: Option<String>,
    pub(super) display: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttemptParseError {
    MissingPrefix,
    MalformedPrefix,
    MissingField,
    DuplicateField,
    InvalidEncoding,
    InvalidIteration,
}

impl AttemptMetadata {
    pub(super) fn from_fields(
        binary_id: Option<&str>,
        test_name: Option<&str>,
        stress_current: Option<&str>,
        run_id: Option<&str>,
        attempt_id: Option<&str>,
    ) -> Result<Self, AttemptParseError> {
        let attempt_id = optional(attempt_id)?;
        let run_id = optional(run_id)?;
        let binary_id = required(binary_id)?;
        let test_name = required(test_name)?;
        let stress_current = required(stress_current)?;
        let iteration = stress_current
            .parse::<usize>()
            .map_err(|_| AttemptParseError::InvalidIteration)?;
        if stress_current != iteration.to_string() {
            return Err(AttemptParseError::InvalidIteration);
        }

        let key = AttemptKey {
            iteration,
            suite: binary_id.to_owned(),
            name: test_name.to_owned(),
        };
        let display = attempt_id.map_or_else(|| render_key(&key), str::to_owned);

        Ok(Self {
            display,
            key,
            run_id: run_id.map(str::to_owned),
        })
    }
}

pub(super) fn split_prefix(line: &str) -> Result<(AttemptMetadata, &str), AttemptParseError> {
    let rest = line
        .strip_prefix("[nextest ")
        .ok_or(AttemptParseError::MissingPrefix)?;
    let end = rest.find("] ").ok_or(AttemptParseError::MalformedPrefix)?;
    let mut fields = BTreeMap::new();
    for field in rest[..end].split_whitespace() {
        let (key, value) = field
            .split_once('=')
            .ok_or(AttemptParseError::MalformedPrefix)?;
        if fields.insert(key, value).is_some() {
            return Err(AttemptParseError::DuplicateField);
        }
    }
    let attempt_id = decode_field(&fields, "attempt_id")?;
    let run_id = decode_field(&fields, "run_id")?;
    let binary_id = decode_field(&fields, "binary_id")?;
    let test_name = decode_field(&fields, "test_name")?;
    let stress_current = decode_field(&fields, "stress_current")?;
    let metadata = AttemptMetadata::from_fields(
        binary_id.as_deref(),
        test_name.as_deref(),
        stress_current.as_deref(),
        run_id.as_deref(),
        attempt_id.as_deref(),
    )?;
    Ok((metadata, &rest[end + 2..]))
}

pub(super) fn attempt_outcomes(cases: &[CaseTiming]) -> BTreeMap<AttemptKey, AttemptOutcome> {
    let mut outcomes = BTreeMap::new();
    for case in cases {
        let Some(iteration) = case.iteration else {
            continue;
        };
        let key = AttemptKey {
            iteration,
            suite: case.suite.clone(),
            name: case.name.clone(),
        };
        let outcome = if case.failing() {
            AttemptOutcome::Failed
        } else {
            AttemptOutcome::Passed
        };
        outcomes
            .entry(key)
            .and_modify(|current| {
                if outcome == AttemptOutcome::Failed {
                    *current = outcome;
                }
            })
            .or_insert(outcome);
    }
    outcomes
}

pub(super) fn outcome_for(
    metadata: &AttemptMetadata,
    outcomes: &BTreeMap<AttemptKey, AttemptOutcome>,
) -> AttemptOutcome {
    outcomes
        .get(&metadata.key)
        .copied()
        .unwrap_or(AttemptOutcome::Unattributed)
}

pub(super) fn foreign_run(metadata: &AttemptMetadata, expected: Option<&str>) -> bool {
    expected.is_some_and(|expected| metadata.run_id.as_deref() != Some(expected))
}

pub(super) fn render_key(key: &AttemptKey) -> String {
    format!("{} {} @stress-{}", key.suite, key.name, key.iteration)
}

pub(super) fn render_test(key: &AttemptKey) -> String {
    format!("{} {}", key.suite, key.name)
}

fn required(value: Option<&str>) -> Result<&str, AttemptParseError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or(AttemptParseError::MissingField)
}

fn optional(value: Option<&str>) -> Result<Option<&str>, AttemptParseError> {
    match value {
        Some("") => Err(AttemptParseError::MissingField),
        value => Ok(value),
    }
}

fn decode_field(
    fields: &BTreeMap<&str, &str>,
    key: &str,
) -> Result<Option<String>, AttemptParseError> {
    fields
        .get(key)
        .map(|value| percent_decode(value).ok_or(AttemptParseError::InvalidEncoding))
        .transpose()
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(
                hex_value(high)?
                    .checked_mul(16)?
                    .checked_add(hex_value(low)?)?,
            );
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_decodes_opaque_display_and_canonical_join_fields() {
        let line = "[nextest run_id=run%20id attempt_id=opaque%2Fretry%23two binary_id=demo%3A%3Atests test_name=seek%20case stress_current=2] [no_block] CPU spin";

        let (metadata, evidence) = split_prefix(line).expect("canonical prefix");

        assert_eq!(metadata.display, "opaque/retry#two");
        assert_eq!(metadata.run_id.as_deref(), Some("run id"));
        assert_eq!(
            metadata.key,
            AttemptKey {
                suite: "demo::tests".to_owned(),
                name: "seek case".to_owned(),
                iteration: 2,
            }
        );
        assert_eq!(evidence, "[no_block] CPU spin");
    }

    #[test]
    fn missing_attempt_id_uses_canonical_display_and_still_joins() {
        let (metadata, _) = split_prefix(
            "[nextest run_id=run binary_id=demo test_name=seek stress_current=2] evidence",
        )
        .expect("prefix without optional attempt ID");

        assert_eq!(metadata.display, "demo seek @stress-2");
        assert_eq!(
            metadata.key,
            AttemptKey {
                suite: "demo".to_owned(),
                name: "seek".to_owned(),
                iteration: 2,
            }
        );
        assert_eq!(metadata.run_id.as_deref(), Some("run"));
    }

    #[test]
    fn run_filter_does_not_inspect_opaque_attempt_id() {
        let matching = AttemptMetadata::from_fields(
            Some("demo"),
            Some("seek"),
            Some("2"),
            Some("run"),
            Some("retry-like-but-opaque#9"),
        )
        .expect("opaque attempt ID");
        let missing =
            AttemptMetadata::from_fields(Some("demo"), Some("seek"), Some("2"), None, None)
                .expect("missing optional metadata");

        assert!(!foreign_run(&matching, Some("run")));
        assert!(foreign_run(&matching, Some("other-run")));
        assert!(foreign_run(&missing, Some("run")));
        assert!(!foreign_run(&missing, None));
    }

    #[test]
    fn malformed_or_partial_prefix_is_rejected() {
        assert!(matches!(
            split_prefix(
                "[nextest run_id=run malformed binary_id=demo test_name=seek stress_current=2] evidence"
            ),
            Err(AttemptParseError::MalformedPrefix)
        ));
        assert!(matches!(
            split_prefix(
                "[nextest attempt_id=run binary_id=demo test_name=seek%2 stress_current=2] evidence"
            ),
            Err(AttemptParseError::InvalidEncoding)
        ));
        assert!(matches!(
            split_prefix("[nextest attempt_id=opaque test_name=seek stress_current=2] evidence"),
            Err(AttemptParseError::MissingField)
        ));
        assert!(matches!(
            split_prefix(
                "[nextest run_id=run run_id=other binary_id=demo test_name=seek stress_current=2] evidence"
            ),
            Err(AttemptParseError::DuplicateField)
        ));
        assert!(matches!(
            split_prefix(
                "[nextest attempt_id= binary_id=demo test_name=seek stress_current=2] evidence"
            ),
            Err(AttemptParseError::MissingField)
        ));
    }
}

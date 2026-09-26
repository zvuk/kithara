use std::io::Error;

use kithara_derive::Patch;
use kithara_test_utils::{kithara, kithara_platform::time::Duration};

mod seconds {
    use serde::{Deserialize, Deserializer};

    use super::Duration;

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Option::<u64>::deserialize(deserializer).map(|value| value.map(Duration::from_secs))
    }
}

#[derive(Clone, Debug, PartialEq, Patch)]
#[patch(validate = Self::validated, error = Error)]
struct Settings {
    limit: u32,
    payload: serde_yaml_ng::Value,
    #[patch(wire = u8, from = usize::from)]
    count: usize,
    optional: Option<u32>,
    #[patch(attribute(serde(with = "seconds")))]
    timeout: Duration,
    #[patch(attribute(serde(
        rename = "pause",
        deserialize_with = "humantime_serde::option::deserialize"
    )))]
    idle: Option<Duration>,
    #[patch(humantime)]
    delay: Option<Duration>,
}

impl Settings {
    fn validated(self) -> Result<Self, Error> {
        if self.limit == 0 {
            return Err(Error::other("limit must be positive"));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Patch)]
#[patch(fallible)]
struct Owner {
    other: Option<u32>,
    #[patch(nested, fallible)]
    settings: Settings,
}

fn settings() -> Settings {
    Settings {
        limit: 4,
        payload: serde_yaml_ng::Value::Bool(true),
        count: 42,
        optional: Some(9),
        timeout: Duration::from_secs(3),
        idle: Some(Duration::from_secs(5)),
        delay: Some(Duration::from_secs(6)),
    }
}

#[kithara::test(native, flash(false))]
fn missing_values_preserve_state_and_present_values_replace_it() {
    let mut config = settings();
    let original = config.clone();
    config
        .apply(serde_yaml_ng::from_str("{}").unwrap())
        .unwrap();
    assert_eq!(config, original);
    config
        .apply(
            serde_yaml_ng::from_str("optional: 7\ntimeout: 2\npause: 4s\ndelay: 7s\ncount: 2")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(config.optional, Some(7));
    assert_eq!(config.count, 2);
    assert_eq!(config.timeout, Duration::from_secs(2));
    assert_eq!(config.idle, Some(Duration::from_secs(4)));
    assert_eq!(config.delay, Some(Duration::from_secs(7)));
}

#[kithara::test(native, flash(false))]
fn null_clears_optional_values_including_nested_humantime() {
    let mut owner = Owner {
        settings: settings(),
        other: Some(11),
    };
    owner
        .apply(
            serde_yaml_ng::from_str("settings:\n  optional: null\n  pause: null\n  delay: null")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(owner.settings.optional, None);
    assert_eq!(owner.settings.idle, None);
    assert_eq!(owner.settings.delay, None);
    assert_eq!(owner.settings.timeout, Duration::from_secs(3));
}

#[kithara::test(native, flash(false))]
fn documents_reject_required_null_and_unknown_fields() {
    for document in [
        "limit: null",
        "timeout: null",
        "count: null",
        "payload: null",
        "extra: 7",
        "optional: text",
    ] {
        assert!(
            serde_yaml_ng::from_str::<SettingsPatch>(document).is_err(),
            "{document}"
        );
    }
}

#[kithara::test(native, flash(false))]
fn rejected_nested_patch_does_not_commit_a_clear() {
    let mut owner = Owner {
        settings: settings(),
        other: Some(11),
    };
    let original = owner.settings.clone();
    let patch = serde_yaml_ng::from_str(
        "other: null\nsettings:\n  limit: 0\n  optional: null\n  pause: null\n  delay: null",
    )
    .unwrap();
    assert!(owner.apply(patch).is_err());
    assert_eq!(owner.settings, original);
    assert_eq!(owner.other, Some(11));
}

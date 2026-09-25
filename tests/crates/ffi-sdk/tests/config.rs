use kithara_config::Config as _;
use kithara_test_utils::kithara;

mod private {
    #[derive(Debug, thiserror::Error)]
    #[error("low must not exceed high")]
    pub(crate) struct ValidationError;

    #[kithara_config::config(default, update)]
    #[derive(Clone, kithara_config::Patch)]
    #[patch(validate = Self::validated, error = ValidationError)]
    pub(crate) struct Validated {
        #[config(value, update)]
        #[builder(default = 1)]
        low: u32,
        #[config(value, update)]
        #[builder(default = 4)]
        high: u32,
    }

    impl Validated {
        fn validated(self) -> Result<Self, ValidationError> {
            (self.low <= self.high)
                .then_some(self)
                .ok_or(ValidationError)
        }
    }

    #[kithara_config::config(default, update)]
    #[derive(Clone, kithara_config::Patch)]
    pub(crate) struct Timing {
        /// Frame distance between events.
        #[config(value, update)]
        #[builder(required, default = Some(3))]
        #[field(get(copy))]
        width: Option<usize>,
        #[cfg_attr(all(), cfg(any()))]
        #[config(value)]
        unavailable: TypeUnavailableInThisProfile,
    }

    #[kithara_config::config]
    pub(crate) struct Session<'a, T, const N: usize>
    where
        T: core::fmt::Debug,
    {
        #[config(skip = "construction resource")]
        #[field(get)]
        resource: T,
        #[config(skip = "borrowed preparation storage")]
        #[field(get)]
        storage: &'a mut [u8; N],
        /// Owned display label.
        #[config(value(String, self.label.to_owned()))]
        label: &'a str,
        #[config(nested)]
        timing: Timing,
    }
}

#[kithara::test(native, flash(false))]
fn external_consumer_reads_private_nested_values_without_cloning_resources() {
    #[derive(Debug)]
    struct Resource;
    let mut storage = [0; 4];
    let timing = private::Timing::default();
    assert_eq!(timing.width(), Some(3));
    let config = private::Session::builder()
        .resource(Resource)
        .storage(&mut storage)
        .label("input")
        .timing(timing)
        .build();
    let snapshot = config.values();
    assert_eq!(snapshot.label, "input");
    assert_eq!(snapshot.timing.width, Some(3));
    assert_eq!(config.storage().len(), 4);
    assert_eq!(format!("{:?}", config.resource()), "Resource");
    drop(config);
    assert_eq!(snapshot.label, "input");
}

#[kithara::test(native, flash(false))]
fn optional_runtime_clear_and_reset_are_distinct_without_touching_resources() {
    use private::{Timing, TimingUpdate, TimingWidthUpdate};

    let mut timing = Timing::default();
    timing.apply_update(TimingUpdate {
        width: TimingWidthUpdate::Clear,
        ..Default::default()
    });
    assert_eq!(timing.width(), None);

    timing.apply_update(TimingUpdate {
        width: TimingWidthUpdate::Reset,
        ..Default::default()
    });
    assert_eq!(timing.width(), Some(3));
}

#[kithara::test(native, flash(false))]
fn rejected_runtime_update_preserves_the_entire_retained_config() {
    use private::{Validated, ValidatedLowUpdate, ValidatedUpdate};

    let mut config = Validated::default();
    let error = config
        .apply_update(ValidatedUpdate {
            low: ValidatedLowUpdate::Set { value: 9 },
            ..Default::default()
        })
        .expect_err("the existing patch validator rejects the candidate");
    assert_eq!(error.to_string(), "low must not exceed high");
    let values = config.values();
    assert_eq!(values.low, 1);
    assert_eq!(values.high, 4);
}

struct Prepared {
    doubled: u32,
}

#[kithara_config::config]
impl Prepared {
    #[builder(start_fn = builder, finish_fn = build)]
    fn new(input: u32) -> Result<Self, &'static str> {
        let doubled = input.checked_mul(2).ok_or("overflow")?;
        Ok(Self { doubled })
    }
}

#[kithara_config::config]
fn length(input: &str) -> usize {
    input.len()
}

#[kithara::test(native, flash(false))]
fn wrapped_function_builders_preserve_preparation_and_errors() {
    assert_eq!(Prepared::builder().input(21).build().unwrap().doubled, 42);
    assert!(Prepared::builder().input(u32::MAX).build().is_err());
    assert_eq!(length().input("abc").call(), 3);
}

use std::io::Error;

use kithara_config::{Config, ConfigOwner, Patch};
use kithara_test_utils::kithara;

#[derive(Clone, Patch, Config)]
#[config(default, debug)]
struct Levels {
    #[config(value, builder(default = 2), get(copy))]
    level: u32,
    #[config(value, builder(required, default = Some(4)))]
    limit: Option<u32>,
}

#[derive(Clone, Patch, Config)]
#[config(validate_builder, patch(validate = Self::validated, error = Error))]
struct Bounded {
    #[config(value, builder(default = 2), get(copy))]
    level: u32,
    #[config(value, builder(required, default = Some(4)))]
    limit: Option<u32>,
}

impl Bounded {
    fn validated(self) -> Result<Self, Error> {
        if self.limit.is_some_and(|limit| self.level > limit) {
            return Err(Error::other("level exceeds the limit"));
        }
        Ok(self)
    }
}

#[kithara::test]
fn a_judged_builder_refuses_what_its_check_refuses() {
    assert!(Bounded::builder().level(5).build().is_err());
    let bounded = Bounded::builder()
        .level(4)
        .build()
        .expect("boundary value is valid");
    assert_eq!(bounded.level(), 4);
}

#[derive(Config)]
#[config(owner_access, builder(none), fields(get(ref)))]
struct GenericResource<T>
where
    T: Send + Sync,
{
    #[config(skip = "borrowed by its runtime owner")]
    resource: T,
}

#[derive(ConfigOwner)]
#[config_owner(GenericResource<T>, inner.config)]
struct GenericOwner<T>
where
    T: Send + Sync,
{
    inner: Box<GenericInner<T>>,
}

struct GenericInner<T>
where
    T: Send + Sync,
{
    config: GenericResource<T>,
}

#[derive(Config)]
#[config(builder(none), fields(value, get(copy)))]
struct ValueFields {
    level: u32,
    #[config(skip = "runtime resource", get(ref))]
    resource: String,
}

#[derive(Config)]
#[config(builder(none), fields(nested, get(ref)))]
struct NestedFields {
    settings: ValueFields,
    #[config(value)]
    label: String,
}

#[derive(Config)]
#[config(construction)]
struct ConstructionInputs {
    resource: String,
    #[config(get(ref))]
    label: String,
}

#[kithara::test]
fn construction_inputs_need_no_field_exclusions() {
    let inputs = ConstructionInputs::builder()
        .resource(String::from("owned"))
        .label(String::from("label"))
        .build();
    assert_eq!(inputs.resource, "owned");
    assert_eq!(inputs.label(), "label");
}

#[kithara::test]
fn type_level_value_role_allows_explicit_resource_exclusion() {
    let config = ValueFields {
        level: 3,
        resource: String::from("owned"),
    };
    assert_eq!(config.level(), 3);
    assert_eq!(config.values().level, 3);
    assert_eq!(config.resource, "owned");
    assert!(std::ptr::eq(config.resource(), &config.resource));
}

#[kithara::test]
fn nested_field_defaults_preserve_value_overrides_and_borrowed_getters() {
    let config = NestedFields {
        settings: ValueFields {
            level: 5,
            resource: String::from("owned"),
        },
        label: String::from("nested"),
    };
    assert!(std::ptr::eq(config.settings(), &config.settings));
    assert!(std::ptr::eq(config.label(), &config.label));
    let values = config.values();
    assert_eq!(values.settings.level, 5);
    assert_eq!(values.label, "nested");
}

#[kithara::test]
fn generic_owner_access_borrows_the_original_resource() {
    let owner = GenericOwner {
        inner: Box::new(GenericInner {
            config: GenericResource {
                resource: String::from("owned"),
            },
        }),
    };
    assert!(std::ptr::eq(owner.resource(), &owner.inner.config.resource));
}

#[derive(Config)]
#[config(debug, fields(nested))]
struct Session<'a> {
    #[config(skip = "borrowed construction resource", debug(skip))]
    resource: &'a str,
    levels: Levels,
    #[config(skip = "derived from the levels it opens with", builder(skip = levels.level()))]
    opened: u32,
}

struct Wrapped<T>(T);

impl<T> Wrapped<T> {
    fn new(value: T) -> Self {
        Self(value)
    }
}

#[derive(Patch, Config)]
#[config(fields(builder(default)))]
struct WrappedConfig {
    #[config(
        value(u32, self.level.0),
        wrap(default = 2, with = Wrapped::new),
        patch(wire = u32, from = Wrapped::new)
    )]
    level: Wrapped<u32>,
}

#[derive(Config, Patch)]
#[config(
    default,
    debug,
    fields(value, get(copy), builder(default = 2), patch(skip), debug(skip))
)]
struct SharedOptions {
    first: u32,
    #[config(builder(default = 3), patch(attribute(serde(rename = "level"))))]
    second: u32,
    #[config(skip = "owned runtime resource", get(skip), builder(default))]
    resource: String,
}

#[kithara::test]
fn shared_field_options_keep_builders_and_patch_exclusions_independent() {
    let mut config = SharedOptions::default();
    assert_eq!(config.first(), 2);
    assert_eq!(config.second(), 3);
    assert_eq!(format!("{config:?}"), "SharedOptions { .. }");
    config.apply(SharedOptionsPatch { second: Some(9) });
    assert_eq!(config.second(), 9);
    assert_eq!(config.first(), 2);
    assert!(config.resource.is_empty());
}

#[kithara::test]
fn retained_values_are_owned_and_resources_stay_private() {
    assert_eq!(Levels::default().level(), 2);
    let levels = Levels {
        level: 7,
        limit: None,
    };
    let session = Session::builder()
        .resource("injected")
        .levels(levels)
        .build();
    let values = session.values();
    assert_eq!(values.levels.level, 7);
    assert_eq!(values.levels.limit, None);
    assert_eq!(session.resource, "injected");
    assert_eq!(
        session.opened, 7,
        "a skipped field reads the builder's arguments"
    );
}

#[kithara::test]
fn debug_prints_every_field_but_the_skipped_ones() {
    let session = Session::builder()
        .resource("secret")
        .levels(Levels::default())
        .build();
    assert_eq!(
        format!("{session:?}"),
        "Session { levels: Levels { level: 2, limit: Some(4) }, opened: 2, .. }"
    );
}

#[kithara::test]
fn wrapped_fields_keep_builder_defaults_setters_and_patch_conversion() {
    let mut config = WrappedConfig::builder().build();
    assert_eq!(config.values().level, 2);

    let configured = WrappedConfig::builder().level(7).build();
    assert_eq!(configured.values().level, 7);

    config.apply(WrappedConfigPatch { level: Some(9) });
    assert_eq!(config.values().level, 9);
}

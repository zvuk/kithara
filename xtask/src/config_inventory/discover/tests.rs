use super::{discover, registrations};

#[test]
fn construction_enum_records_each_variant_without_claiming_retained_values() {
    let entries = registrations(
        "crates/kithara-host/src/host/config.rs",
        r#"
        #[kithara_config::config(construction, builder = false)]
        pub enum HostConfig<S> {
            #[config(sdk)]
            Realtime {
                #[config(value)] rate: u32,
                #[config(skip = "type marker")] marker: S,
            },
            #[cfg(feature = "offline")]
            Offline {
                #[config(nested)] worker: WorkerConfig,
            },
        }
        "#,
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].owner, "HostConfig::Realtime");
    assert_eq!(entries[0].kind, "construction");
    assert!(entries[0].sdk);
    assert_eq!(entries[0].fields[0].role, "value");
    assert!(entries[0].fields[0].value_type.is_none());
    assert_eq!(entries[1].owner, "HostConfig::Offline");
    assert!(!entries[1].sdk);
    assert_eq!(entries[1].fields[0].role, "nested");
    assert_eq!(entries[1].conditions, ["# [cfg (feature = \"offline\")]"]);
}

#[test]
fn registrations_separate_retained_values_from_delegated_operations() {
    let entries = registrations(
        "crates/player/src/control.rs",
        r#"
        /// A retained recipe.
        #[kithara_config::config(builder = false)]
        struct EqConfig<S> {
            /// Smoothing policy.
            #[config(value)] smoothing: SmootherConfig,
            #[config(skip = "injected pool")] pools: S,
        }
        impl<S> PlayerControl<S> {
            /// Replace the live layout.
            #[kithara_config::config(delegate = "eq_layout", sdk)]
            fn set_eq_layout(&self, layout: Vec<EqBandConfig>) -> Result<(), Error> { todo!() }
        }
        "#,
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].package, "player");
    assert_eq!(entries[0].module_path, "control");
    assert_eq!(entries[0].kind, "retained");
    assert_eq!(entries[0].fields[0].role, "value");
    assert_eq!(entries[0].fields[1].role, "skip");
    assert_eq!(
        entries[0].fields[1].exclusion_reason.as_deref(),
        Some("injected pool")
    );
    assert_eq!(entries[1].kind, "delegate");
    assert_eq!(entries[1].owner, "PlayerControl < S >");
    assert_eq!(entries[1].property.as_deref(), Some("eq_layout"));
    assert_eq!(entries[1].hook.as_deref(), Some("set_eq_layout"));
    assert!(entries[1].sdk);
    assert_eq!(entries[1].fields[0].rust_type, "Vec < EqBandConfig >");
    assert!(
        entries[1]
            .docs
            .iter()
            .any(|line| line.contains("live layout"))
    );
}

#[test]
fn construction_input_is_registered_without_claiming_retained_values() {
    let entries = registrations(
        "crates/kithara-audio/src/pipeline/config/audio.rs",
        r#"
        #[kithara_config::config(construction, builder = false)]
        struct AudioConfig<T> {
            #[config(nested, builder(start_fn))] stream: T,
            #[config(value, builder(default = 10))] chunks: usize,
            #[config(skip = "injected observer", patch(skip))] observer: Option<Observer>,
        }
        "#,
    )
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "construction");
    assert_eq!(entries[0].fields.len(), 3);
    assert_eq!(entries[0].fields[0].role, "nested");
    assert!(entries[0].fields[0].value_type.is_none());
    assert_eq!(entries[0].fields[1].builder_default.as_deref(), Some("10"));
    assert!(entries[0].fields[1].value_type.is_none());
    assert_eq!(
        entries[0].fields[2].exclusion_reason.as_deref(),
        Some("injected observer")
    );
}

#[test]
fn live_recorder_registers_consumed_inputs_without_exposing_resources() {
    let source = include_str!("../../../../crates/kithara-record/src/config.rs");
    let entries = registrations("crates/kithara-record/src/config.rs", source).unwrap();
    let live = entries
        .iter()
        .find(|entry| entry.owner == "LiveRecordingConfig")
        .unwrap();
    assert_eq!(live.kind, "construction");
    assert!(!live.sdk);
    assert_eq!(live.fields.len(), 16);
    assert!(live.fields.iter().all(|field| field.value_type.is_none()));
    assert_eq!(live.fields[0].role, "skip");
    assert_eq!(live.fields[1].role, "skip");
    assert_eq!(live.fields[2].role, "skip");
    assert_eq!(live.fields[10].name, "generation_capacity");
    assert_eq!(live.fields[10].role, "value");
    assert_eq!(live.fields[15].name, "recording");
    assert_eq!(live.fields[15].role, "nested");
}

#[test]
fn analysis_worker_registers_prepared_inputs_without_claiming_live_values() {
    let source = include_str!("../../../../crates/kithara-analysis/src/worker/config.rs");
    let entries = registrations("crates/kithara-analysis/src/worker/config.rs", source).unwrap();
    let worker = entries
        .iter()
        .find(|entry| entry.owner == "AnalysisWorkerConfig")
        .unwrap();
    assert_eq!(worker.kind, "construction");
    assert!(!worker.sdk);
    assert_eq!(worker.fields.len(), 14);
    assert!(worker.fields.iter().all(|field| field.value_type.is_none()));
    let roles: Vec<_> = worker
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.role.as_str()))
        .collect();
    assert_eq!(roles[0], ("builder", "skip"));
    assert_eq!(roles[4], ("chunk_seconds", "value"));
    assert_eq!(roles[11], ("cancel", "skip"));
    assert_eq!(roles[12], ("worker", "skip"));
    assert_eq!(roles[13], ("priority", "value"));
}

#[test]
fn playback_worker_registers_dispatcher_inputs_without_exposing_resources() {
    let source = include_str!("../../../../crates/kithara-play/src/worker/config.rs");
    let entries = registrations("crates/kithara-play/src/worker/config.rs", source).unwrap();
    let worker = entries
        .iter()
        .find(|entry| entry.owner == "PlayWorkerConfig")
        .unwrap();
    assert_eq!(worker.kind, "construction");
    assert!(!worker.sdk);
    assert_eq!(worker.fields.len(), 10);
    assert!(worker.fields.iter().all(|field| field.value_type.is_none()));
    let roles: Vec<_> = worker
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.role.as_str()))
        .collect();
    assert_eq!(roles[0], ("pools", "skip"));
    assert!(roles[1..8].iter().all(|(_, role)| *role == "value"));
    assert_eq!(roles[8], ("cancel", "skip"));
    assert_eq!(roles[9], ("worker", "skip"));
}

#[test]
fn manifest_reads_composed_builder_field_and_patch_groups() {
    let entries = registrations(
        "crates/kithara-play/src/player/config.rs",
        r#"
        #[kithara_config::config(builder = false)]
        struct PlayerConfig {
            #[config(value, builder(default = Consts::MAX_BAR_RATIO), field(get, copy))]
            rate: u32,
            #[config(skip = "injected resource", builder(default), patch(skip))]
            resource: Option<u32>,
            #[cfg(feature = "web")]
            #[config(value)]
            #[builder(default = 4)]
            count: u32,
            #[config(value(Option<u64>, self.optional.map(u64::from)), builder(default))]
            optional: Option<u32>,
            #[config(nested)]
            child: ChildConfig,
        }
        "#,
    )
    .unwrap();
    assert_eq!(entries[0].fields[0].role, "value");
    assert_eq!(
        entries[0].fields[0].builder_default.as_deref(),
        Some("Consts :: MAX_BAR_RATIO")
    );
    assert_eq!(entries[0].fields[1].role, "skip");
    assert_eq!(
        entries[0].fields[1].builder_default.as_deref(),
        Some("default")
    );
    assert_eq!(
        entries[0].fields[1].exclusion_reason.as_deref(),
        Some("injected resource")
    );
    assert_eq!(entries[0].fields[2].builder_default.as_deref(), Some("4"));
    assert_eq!(
        entries[0].fields[2].conditions,
        ["# [cfg (feature = \"web\")]"]
    );
    assert_eq!(entries[0].fields[3].role, "value");
    assert_eq!(entries[0].fields[3].rust_type, "Option < u32 >");
    assert_eq!(
        entries[0].fields[3].value_type.as_deref(),
        Some("Option < u64 >")
    );
    assert_eq!(
        entries[0].fields[3].builder_default.as_deref(),
        Some("default")
    );
    assert_eq!(
        entries[0].fields[4].value_type.as_deref(),
        Some("<ChildConfig as kithara_config::Config>::Values")
    );
}

#[test]
fn schema_modules_and_patch_derives_do_not_require_config_suffixes() {
    let source = "struct Ordinary; mod config { struct Limits; } #[derive(Patch)] struct Recipe; mod other { struct Hidden; }";
    let entries = discover("src/lib.rs", source).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Limits", "Recipe"]
    );
    for path in [
        "src/config.rs",
        "src/config/host.rs",
        "src/document/schema.rs",
    ] {
        assert_eq!(
            discover(path, "enum Mode { Fast } type Configuration = Mode;")
                .unwrap()
                .len(),
            2
        );
    }
    assert!(
        discover("src/configuration_cache.rs", "struct Ordinary;")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn config_bearing_inputs_are_detected_without_bon() {
    let entries = discover("src/lib.rs", "impl Player { fn new(config: &AudioConfig, mode: Mode) -> Self { todo!() } fn ordinary(value: usize) {} } fn prepare(input: Option<Settings>) {} type Configuration = AudioConfig;").unwrap();
    assert_eq!(
        entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
        ["config_inputs", "config_inputs", "alias"]
    );
    assert_eq!(
        entries[0].members,
        ["config : & AudioConfig", "mode : Mode"]
    );
}

#[test]
fn file_conditions_and_associated_aliases_are_not_lost() {
    let entries = discover("src/native.rs", "#![cfg(unix)] impl Service for Audio { type Config = Options; } fn test() { struct LocalConfig; }").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "associated_alias");
    assert_eq!(entries[0].members, ["Options"]);
    assert_eq!(entries[0].conditions, ["# ! [cfg (unix)]"]);
    assert_eq!(entries[1].conditions, entries[0].conditions);
    assert_eq!(entries[1].scope, ["test"]);
}

#[test]
fn additions_are_detected_without_export_registration() {
    let before = discover("src/lib.rs", "struct Config { value: u32 }").unwrap();
    let after = discover(
        "src/lib.rs",
        "struct Config { value: u32, extra: Option<u64> } struct NewConfig;",
    )
    .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 2);
    assert_ne!(before[0].members, after[0].members);
    assert_eq!(after[1].name, "NewConfig");
}

#[test]
fn scopes_conditions_and_constructor_only_inputs_survive_discovery() {
    let entries = discover(
        "src/lib.rs",
        r#"
        #[cfg(feature = "audio")]
        mod audio {
            struct Config<T> { #[cfg(unix)] value: T }
            #[bon]
            impl<T> Config<T> {
                #[builder]
                fn new(value: T, ephemeral: usize) -> Self { todo!() }
            }
        }
        mod other { struct Config; }
        const TEXT: &str = "struct FakeConfig;";
    "#,
    )
    .unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].scope, ["audio"]);
    assert_eq!(entries[2].scope, ["other"]);
    assert_eq!(entries[0].members, ["# [cfg (unix)] value : T"]);
    assert_eq!(entries[1].kind, "builder_inputs");
    assert_eq!(entries[1].members, ["value : T", "ephemeral : usize"]);
    assert_eq!(entries[0].conditions, entries[1].conditions);
    assert!(!entries[0].conditions.is_empty());
    assert!(entries[2].conditions.is_empty());
}

#[test]
fn enums_aliases_and_builder_names_are_candidates() {
    let entries = discover("src/lib.rs", "enum InputConfig { Value { count: u32 }, Empty } type AliasConfig = InputConfig; #[derive(bon::Builder)] struct Recipe { count: u32 }").unwrap();
    assert_eq!(
        entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
        ["enum", "alias", "struct"]
    );
    assert_eq!(entries[0].members, ["Value { count : u32 }", "Empty"]);
    assert_eq!(entries[1].members, ["InputConfig"]);
    assert!(discover("broken.rs", "struct BadConfig {").is_err());
}

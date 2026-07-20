use std::{collections::BTreeMap, hint::black_box};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    ids::{DocId, EndpointId, NodeId, SourceUri},
    module::{
        AdaptivePolicy, BindingRef, ButtonStyle, ControlNode, FaderStyle, ModuleDoc, ScalarFormat,
        WaveStyle, parse_module,
    },
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{
        ControlAction, ReadValue, Reads, StereoLevels, TrackRow, UiEvent, WaveBucket, WaveformView,
        tree,
    },
    source::{Limits, MemResolver, UiConfig},
};
use num_traits::cast::AsPrimitive;

const MODULE_SKELETON: &str = r#"(
    schema: "kithara.module",
    version: 1,
    id: "bench",
    root: Column(children: []),
)"#;
const TRACKS: [TrackRow<'static>; 3] = [
    TrackRow {
        title: "Signal Path",
        artist: Some("Kithara"),
        time: Some("04:12"),
        search: Some("signal path kithara"),
        current: true,
        selected: true,
    },
    TrackRow {
        title: "Four Decks",
        artist: Some("Benchmark"),
        time: Some("05:31"),
        search: Some("four decks benchmark"),
        current: false,
        selected: false,
    },
    TrackRow {
        title: "Continuous Drag",
        artist: Some("Criterion"),
        time: Some("03:47"),
        search: Some("continuous drag criterion"),
        current: false,
        selected: false,
    },
];

struct Fixture {
    resolver: MemResolver,
    registry: BenchRegistry,
    config: UiConfig,
}

impl Fixture {
    fn compile(&self) -> CompiledUi {
        compile(
            "bench.klayout.ron",
            &self.resolver,
            &self.registry,
            builtin::skin_doc(),
            &self.config,
        )
        .unwrap_or_else(|error| panic!("benchmark fixture must compile: {error}"))
    }
}

#[derive(Default)]
struct BenchRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl BenchRegistry {
    fn insert(&mut self, id: &str, kind: ValueKind) {
        self.endpoints.insert(
            (EndpointCategory::Model, EndpointId(id.to_owned())),
            EndpointDesc::new(kind),
        );
    }
}

impl EndpointRegistry for BenchRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

struct BenchReads {
    levels: [StereoLevels; 8],
    phase: f32,
    scalar: f64,
    waveforms: [Vec<WaveBucket>; 4],
}

impl BenchReads {
    fn new() -> Self {
        Self {
            levels: [StereoLevels::default(); 8],
            phase: 0.0,
            scalar: 0.7,
            waveforms: std::array::from_fn(waveform),
        }
    }

    fn apply(&mut self, event: &UiEvent) {
        let UiEvent::Control { path, action } = event else {
            return;
        };
        let ControlAction::SetScalar(value) = action else {
            return;
        };
        if path == "nested/level-0/fader-0-0" {
            self.scalar = value.clamp(0.0, 1.0);
        }
    }

    fn push(&mut self) {
        self.phase += 0.037;
        for (index, waveform) in self.waveforms.iter_mut().enumerate() {
            waveform.rotate_left(1);
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            if let Some(bucket) = waveform.last_mut() {
                *bucket = wave_bucket(self.phase + offset * 0.71);
            }
        }
        for (index, levels) in self.levels.iter_mut().enumerate() {
            let offset = u16::try_from(index).map_or(0.0, f32::from);
            let carrier = (self.phase * 2.3 + offset * 0.47).sin();
            let noise = (self.phase * 31.7 + offset * 7.13).sin();
            levels.l = (carrier.mul_add(0.32, noise * 0.08 + 0.54)).clamp(0.0, 1.0);
            levels.r = ((carrier + 0.63).sin().mul_add(0.3, noise * 0.09 + 0.5)).clamp(0.0, 1.0);
            levels.volume = self.scalar.as_();
        }
    }
}

impl Reads for BenchReads {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let value = match endpoint {
            "bench.bool" => ReadValue::Bool(true),
            "bench.scalar" => ReadValue::Scalar(self.scalar),
            "bench.text" => ReadValue::Text("Kithara benchmark"),
            "bench.tracks" => ReadValue::TrackList(&TRACKS),
            "bench.wave.0" => waveform_value(&self.waveforms[0]),
            "bench.wave.1" => waveform_value(&self.waveforms[1]),
            "bench.wave.2" => waveform_value(&self.waveforms[2]),
            "bench.wave.3" => waveform_value(&self.waveforms[3]),
            "bench.level.0" => ReadValue::Stereo(self.levels[0]),
            "bench.level.1" => ReadValue::Stereo(self.levels[1]),
            "bench.level.2" => ReadValue::Stereo(self.levels[2]),
            "bench.level.3" => ReadValue::Stereo(self.levels[3]),
            "bench.level.4" => ReadValue::Stereo(self.levels[4]),
            "bench.level.5" => ReadValue::Stereo(self.levels[5]),
            "bench.level.6" => ReadValue::Stereo(self.levels[6]),
            "bench.level.7" => ReadValue::Stereo(self.levels[7]),
            _ => return None,
        };
        Some(value)
    }
}

fn bench_compile_expand(c: &mut Criterion) {
    let mut group = c.benchmark_group("ui/compile_expand");
    for depth in [1, 4, 8] {
        for width in [10, 100, 300] {
            let fixture = nested_fixture(depth, width, false);
            group.bench_with_input(
                BenchmarkId::new(format!("depth-{depth}"), format!("width-{width}")),
                &(depth, width),
                |b, _| {
                    b.iter(|| black_box(fixture.compile()));
                },
            );
        }
    }
    group.finish();
}

fn bench_view_build(c: &mut Criterion) {
    let ui = realistic_fixture().compile();
    let reads = BenchReads::new();
    c.bench_function("ui/view_build/250-controls", |b| {
        b.iter(|| {
            black_box(tree::render(&ui.root, &ui, &reads, builtin::skin()));
        });
    });
}

fn bench_data_push(c: &mut Criterion) {
    let ui = realistic_fixture().compile();
    let mut reads = BenchReads::new();
    c.bench_function("ui/data_push/8192-wave-8-stereo-view", |b| {
        b.iter(|| {
            reads.push();
            black_box(tree::render(&ui.root, &ui, &reads, builtin::skin()));
        });
    });
}

fn bench_event_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("ui/event_apply");
    for depth in [1, 4, 8] {
        let ui = nested_fixture(depth, 10, true).compile();
        let mut reads = BenchReads::new();
        let event = UiEvent::Control {
            path: "nested/level-0/fader-0-0".to_owned(),
            action: ControlAction::SetScalar(0.63),
        };
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                reads.apply(black_box(&event));
                black_box(tree::render(&ui.root, &ui, &reads, builtin::skin()));
            });
        });
    }
    group.finish();
}

fn nested_fixture(depth: usize, width: usize, bound: bool) -> Fixture {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "bench.klayout.ron",
        r#"(
            schema: "kithara.layout",
            version: 1,
            id: "bench-nested",
            root: Module(instance: "nested", source: "level-0.kmodule.ron"),
        )"#,
    );
    for level in 0..depth {
        let mut children: Vec<_> = (0..width)
            .map(|index| {
                if bound {
                    fader(&format!("fader-{level}-{index}"), "bench.scalar")
                } else {
                    spacer(&format!("spacer-{level}-{index}"))
                }
            })
            .collect();
        if level + 1 < depth {
            children.push(ControlNode::Include {
                id: NodeId(format!("include-{level}")),
                source: format!("level-{}.kmodule.ron", level + 1),
                with: BTreeMap::new(),
            });
        }
        let root = column(Some(format!("level-{level}")), children);
        resolver.insert(
            &format!("level-{level}.kmodule.ron"),
            &module_text(&format!("level-{level}"), root),
        );
    }
    Fixture {
        resolver,
        registry: registry(),
        config: config(depth),
    }
}

fn realistic_fixture() -> Fixture {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "bench.klayout.ron",
        r#"(
            schema: "kithara.layout",
            version: 1,
            id: "bench-realistic",
            root: Module(instance: "dj", source: "dj.kmodule.ron"),
        )"#,
    );
    resolver.insert("dj.kmodule.ron", &module_text("dj", realistic_root()));
    Fixture {
        resolver,
        registry: registry(),
        config: config(8),
    }
}

fn realistic_root() -> ControlNode {
    let mut modules: Vec<_> = (0..4).map(deck).collect();
    modules.push(module_controls("mixer", 49, Vec::new()));
    modules.push(module_controls(
        "library",
        41,
        vec![track_list("tracks", "bench.tracks")],
    ));
    row(Some("dj".to_owned()), modules)
}

fn deck(index: usize) -> ControlNode {
    let level = index * 2;
    module_controls(
        &format!("deck-{index}"),
        40,
        vec![
            wave(&format!("wave-{index}"), &format!("bench.wave.{index}")),
            vu(
                &format!("level-left-{index}"),
                &format!("bench.level.{level}"),
            ),
            vu(
                &format!("level-right-{index}"),
                &format!("bench.level.{}", level + 1),
            ),
        ],
    )
}

fn module_controls(scope: &str, count: usize, mut children: Vec<ControlNode>) -> ControlNode {
    children.extend((children.len()..count).map(|index| generic_control(scope, index)));
    column(Some(scope.to_owned()), children)
}

fn generic_control(scope: &str, index: usize) -> ControlNode {
    let id = format!("{scope}-{index}");
    match index % 4 {
        0 => knob(&id, "bench.scalar"),
        1 => fader(&id, "bench.scalar"),
        2 => scalar(&id, "bench.scalar"),
        _ => button(&id, "bench.bool"),
    }
}

fn module_text(id: &str, root: ControlNode) -> String {
    let origin = SourceUri(format!("{id}.kmodule.ron"));
    let mut document: ModuleDoc = parse_module(MODULE_SKELETON, &origin)
        .unwrap_or_else(|error| panic!("module skeleton must parse: {error}"));
    document.id = DocId(id.to_owned());
    document.root = root;
    ron::ser::to_string(&document)
        .unwrap_or_else(|error| panic!("benchmark module must serialize: {error}"))
}

fn config(depth: usize) -> UiConfig {
    let limits = Limits::builder()
        .max_bytes(2 * 1024 * 1024)
        .max_depth(depth.max(8))
        .max_nodes(20_000)
        .build();
    UiConfig::builder()
        .limits(limits)
        .max_arena_bytes(2 * 1024 * 1024)
        .build()
}

fn registry() -> BenchRegistry {
    let mut registry = BenchRegistry::default();
    for (id, kind) in [
        ("bench.bool", ValueKind::Bool),
        ("bench.scalar", ValueKind::Scalar),
        ("bench.text", ValueKind::Text),
        ("bench.tracks", ValueKind::TrackList),
    ] {
        registry.insert(id, kind);
    }
    for index in 0..4 {
        registry.insert(&format!("bench.wave.{index}"), ValueKind::Waveform);
    }
    for index in 0..8 {
        registry.insert(&format!("bench.level.{index}"), ValueKind::Stereo);
    }
    registry
}

fn model(id: &str) -> BindingRef {
    BindingRef::Model {
        id: EndpointId(id.to_owned()),
        with: BTreeMap::new(),
    }
}

fn row(id: Option<String>, children: Vec<ControlNode>) -> ControlNode {
    ControlNode::Row {
        id: id.map(NodeId),
        size: None,
        gap: None,
        pad: None,
        children,
    }
}

fn column(id: Option<String>, children: Vec<ControlNode>) -> ControlNode {
    ControlNode::Column {
        id: id.map(NodeId),
        size: None,
        gap: None,
        pad: None,
        children,
    }
}

fn spacer(id: &str) -> ControlNode {
    ControlNode::Spacer {
        id: NodeId(id.to_owned()),
        size: None,
        read: None,
        write: None,
        adaptive: AdaptivePolicy::default(),
    }
}

fn knob(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::Knob {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
    }
}

fn fader(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::Fader {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
        style: FaderStyle::default(),
    }
}

fn scalar(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::Scalar {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
        format: ScalarFormat::default(),
    }
}

fn button(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::Button {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
        label: "PLAY".to_owned(),
        active_label: Some("PAUSE".to_owned()),
        style: ButtonStyle::default(),
    }
}

fn wave(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::Wave {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
        style: WaveStyle::default(),
        badge: None,
    }
}

fn vu(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::VuStereo {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
    }
}

fn track_list(id: &str, endpoint: &str) -> ControlNode {
    ControlNode::TrackList {
        id: NodeId(id.to_owned()),
        size: None,
        read: Some(model(endpoint)),
        write: None,
        adaptive: AdaptivePolicy::default(),
    }
}

fn waveform(index: usize) -> Vec<WaveBucket> {
    let offset = u16::try_from(index).map_or(0.0, f32::from);
    (0_u16..8_192)
        .map(|bucket| wave_bucket(f32::from(bucket).mul_add(0.013, offset)))
        .collect()
}

fn wave_bucket(phase: f32) -> WaveBucket {
    WaveBucket {
        low: phase.sin().mul_add(0.34, 0.52).clamp(0.0, 1.0),
        mid: (phase * 1.73).sin().mul_add(0.29, 0.45).clamp(0.0, 1.0),
        high: (phase * 3.11).sin().mul_add(0.2, 0.34).clamp(0.0, 1.0),
    }
}

fn waveform_value(waveform: &[WaveBucket]) -> ReadValue<'_> {
    ReadValue::Waveform(WaveformView {
        buckets: waveform,
        beats: &[],
        downbeats: &[],
        bpm: None,
    })
}

criterion_group!(
    benches,
    bench_compile_expand,
    bench_view_build,
    bench_data_push,
    bench_event_apply,
);
criterion_main!(benches);

use kithara_test_utils::kithara;

use super::{ShaderFrame, frame::ShaderFrameError};
use crate::{
    builtin,
    compile::{CompiledNode, CompiledUi, compile},
    expand::{ControlSpec, ExpandedNode},
    ids::EndpointId,
    registry::{BuiltinEndpoints, EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
    render::{ReadValue, Reads, StereoLevels},
    shader::ShaderSpec,
    source::{MemResolver, UiConfig},
};

const FRAGMENT: &str = r"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(kithara.level.x, kithara.meter.y, position.x / kithara.viewport.x, 1.0);
}
";

/// One endpoint per supported kind, so the packing under test is the one the
/// document actually reaches: a fraction and a pair of levels.
#[derive(Default)]
struct Fixture {
    level: f64,
    meter: StereoLevels,
    wrong_kind: bool,
}

impl Reads for Fixture {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "shader.level" if self.wrong_kind => Some(ReadValue::Text("loud")),
            "shader.level" => Some(ReadValue::Scalar(self.level)),
            "shader.meter" => Some(ReadValue::Stereo(self.meter)),
            _ => None,
        }
    }
}

struct Endpoints {
    level: EndpointDesc,
    meter: EndpointDesc,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            level: EndpointDesc::new(ValueKind::Scalar),
            meter: EndpointDesc::new(ValueKind::Stereo),
        }
    }
}

impl EndpointRegistry for Endpoints {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        if category != EndpointCategory::Model {
            return None;
        }
        match id.0.as_str() {
            "shader.level" => Some(&self.level),
            "shader.meter" => Some(&self.meter),
            _ => None,
        }
    }
}

fn document() -> CompiledUi {
    bound(r#""level": Model(id: "shader.level")"#)
}

/// The same one-shader page, with whatever the caller binds the first uniform
/// to. The second is always the pair of levels, so the packing tests keep both
/// kinds.
fn bound(level: &str) -> CompiledUi {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "shader.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "shader-doc",
            root: Module(instance: "page", source: "page.kmodule.ron"))"#,
    );
    resolver.insert(
        "page.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "page",
            root: Shader(
                id: "field",
                source: "field.wgsl",
                uniforms: {{
                    {level},
                    "meter": Model(id: "shader.meter"),
                }},
                size: (w: Fill, h: Fill),
            ))"#
        ),
    );
    resolver.insert("field.wgsl", FRAGMENT);
    compile(
        "shader.klayout.ron",
        &resolver,
        &BuiltinEndpoints::new(&Endpoints::default()),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
    )
    .unwrap_or_else(|cause| panic!("the fixture document must compile: {cause}"))
}

/// A shader draws its uniforms, so a page whose uniforms hold still draws one
/// picture however often it is asked. Declaring motion for the control's sake
/// costs the window every frame it has: it rasterises and presents the same
/// bytes sixty times a second and never sleeps.
#[kithara::test]
fn a_shader_bound_to_still_endpoints_declares_no_motion() {
    assert!(!document().animates);
}

/// The other half of the same rule: bound to the host's own clock, the very
/// same shader does move with nothing else changing, and the document says so.
#[kithara::test]
fn a_shader_bound_to_the_host_clock_declares_motion() {
    assert!(bound(r#""level": Model(id: "ui.clock.seconds")"#).animates);
}

fn spec(ui: &CompiledUi) -> ShaderSpec {
    fn find(node: &ExpandedNode) -> Option<ShaderSpec> {
        match node {
            ExpandedNode::Control {
                spec: ControlSpec::Shader(spec),
                ..
            } => Some(spec.clone()),
            ExpandedNode::Object { child, .. }
            | ExpandedNode::Optional { child, .. }
            | ExpandedNode::Pressable { child, .. }
            | ExpandedNode::Scroll { child, .. } => find(child),
            ExpandedNode::Popover { anchor, .. } => find(anchor),
            ExpandedNode::Row { children, .. }
            | ExpandedNode::Column { children, .. }
            | ExpandedNode::Slot { children, .. }
            | ExpandedNode::Stage { children, .. } => children.iter().find_map(find),
            ExpandedNode::Control { .. } => None,
        }
    }
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("the fixture root is one module");
    };
    find(root).unwrap_or_else(|| panic!("the fixture declares one shader"))
}

fn packed(frame: &ShaderFrame) -> Vec<u8> {
    let mut bytes = Vec::new();
    frame.write_uniforms([320.0, 180.0], &mut bytes);
    bytes
}

/// The whole point of a document shader: what the host reports reaches the
/// uniform block. A shader that compiled but drew the same bytes forever would
/// pass a capture and still be broken.
#[kithara::test]
fn moving_an_endpoint_moves_the_uniform_block() {
    let ui = document();
    let spec = spec(&ui);
    let read = |level, meter| {
        ShaderFrame::read(
            &spec,
            "page/field",
            &Fixture {
                level,
                meter,
                wrong_kind: false,
            },
            &ui,
        )
        .unwrap_or_else(|cause| panic!("the fixture reads must satisfy the shader: {cause}"))
    };
    let quiet = read(
        0.25,
        StereoLevels {
            l: 0.1,
            r: 0.2,
            volume: 0.3,
        },
    );

    let loud = read(
        0.75,
        StereoLevels {
            l: 0.1,
            r: 0.2,
            volume: 0.3,
        },
    );
    assert_ne!(
        packed(&quiet),
        packed(&loud),
        "the scalar must reach the block"
    );

    let peaked = read(
        0.25,
        StereoLevels {
            l: 0.9,
            r: 0.8,
            volume: 0.7,
        },
    );
    assert_ne!(
        packed(&quiet),
        packed(&peaked),
        "the levels must reach it too"
    );
}

/// The image identity is the control's, not the values': a frame that minted a
/// new one per read would rebuild the GPU target every time a number moved.
#[kithara::test]
fn the_image_identity_survives_a_value_change() {
    let ui = document();
    let spec = spec(&ui);
    let read = |level| {
        ShaderFrame::read(
            &spec,
            "page/field",
            &Fixture {
                level,
                ..Fixture::default()
            },
            &ui,
        )
        .unwrap_or_else(|cause| panic!("the fixture reads must satisfy the shader: {cause}"))
    };

    assert_eq!(read(0.1).image(), read(0.9).image());
    assert_eq!(read(0.1).source(), read(0.9).source());
}

/// The viewport rides in the same block, so a resized pass writes new bytes
/// without re-reading a single endpoint.
#[kithara::test]
fn the_viewport_is_part_of_the_block() {
    let ui = document();
    let frame = ShaderFrame::read(&spec(&ui), "page/field", &Fixture::default(), &ui)
        .unwrap_or_else(|cause| panic!("the fixture reads must satisfy the shader: {cause}"));
    let mut small = Vec::new();
    let mut large = Vec::new();

    frame.write_uniforms([320.0, 180.0], &mut small);
    frame.write_uniforms([640.0, 360.0], &mut large);

    assert_ne!(small, large);
    assert_eq!(small.len(), frame.uniform_byte_len());
}

/// A wrong kind is a typed failure rather than a zero. Degrading here would
/// draw a plausible picture from a binding the document got wrong.
#[kithara::test]
fn a_wrongly_typed_endpoint_names_itself_instead_of_degrading() {
    let ui = document();

    let error = ShaderFrame::read(
        &spec(&ui),
        "page/field",
        &Fixture {
            wrong_kind: true,
            ..Fixture::default()
        },
        &ui,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ShaderFrameError::Kind {
            ref uniform,
            actual: "Text",
            ..
        } if uniform == "level"
    ));
}

/// An endpoint nobody answers is the same kind of failure: the shader does not
/// run at all rather than running against whatever the buffer held last.
#[kithara::test]
fn an_unanswered_endpoint_stops_the_frame() {
    let ui = document();

    let error = ShaderFrame::read(&spec(&ui), "page/field", &Silent, &ui).unwrap_err();

    assert!(matches!(error, ShaderFrameError::Missing { .. }));
}

struct Silent;

impl Reads for Silent {
    fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
        None
    }
}

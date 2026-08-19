#[cfg(feature = "render")]
use {kithara_platform::sync::Arc, std::collections::BTreeMap};

use crate::{
    error::UiDocError,
    expand::Binding,
    ids::{InternId, SourceUri},
    registry::ValueKind,
};

/// One document endpoint packed into the shader's standard uniform block.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShaderUniform {
    pub(crate) kind: ValueKind,
    pub(crate) name: InternId,
    pub(crate) read: Binding,
}

/// Validated WGSL and the endpoint bindings that feed it.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct ShaderSpec {
    #[cfg(feature = "render")]
    module: Arc<ShaderModule>,
    #[field(get, vis = "pub(crate)")]
    uniforms: Vec<ShaderUniform>,
}

#[cfg(feature = "render")]
impl ShaderSpec {
    pub(crate) fn source(&self) -> Arc<str> {
        Arc::clone(&self.module.source)
    }
}

#[cfg(feature = "render")]
#[derive(Debug, PartialEq)]
struct ShaderModule {
    origin: SourceUri,
    source: Arc<str>,
}

#[cfg(feature = "render")]
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ShaderKey {
    fields: Vec<String>,
    origin: SourceUri,
}

#[derive(Default)]
pub(crate) struct ShaderCache {
    #[cfg(feature = "render")]
    modules: BTreeMap<ShaderKey, Arc<ShaderModule>>,
}

pub(crate) fn compile(
    cache: &mut ShaderCache,
    authored: &str,
    origin: &SourceUri,
    path: &str,
    uniforms: Vec<ShaderUniform>,
    fields: &[String],
) -> Result<ShaderSpec, UiDocError> {
    #[cfg(not(feature = "render"))]
    {
        drop((cache, authored, uniforms, fields));
        return Err(error(
            origin,
            path,
            "Shader controls require the render feature".to_owned(),
        ));
    }
    #[cfg(feature = "render")]
    {
        let key = ShaderKey {
            fields: fields.to_vec(),
            origin: origin.clone(),
        };
        if let Some(module) = cache.modules.get(&key) {
            return Ok(ShaderSpec {
                module: Arc::clone(module),
                uniforms,
            });
        }
        let source = wgsl::combined(authored, origin, path, fields)?;
        let module = naga::front::wgsl::parse_str(&source)
            .map_err(|cause| error(origin, path, cause.to_string()))?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .map_err(|cause| error(origin, path, cause.to_string()))?;
        for (stage, name) in [
            (naga::ShaderStage::Vertex, "vs_main"),
            (naga::ShaderStage::Fragment, "fs_main"),
        ] {
            if !module
                .entry_points
                .iter()
                .any(|entry| entry.stage == stage && entry.name == name)
            {
                return Err(error(
                    origin,
                    path,
                    format!("missing {stage:?} entry point {name:?}"),
                ));
            }
        }
        let resources = module
            .global_variables
            .iter()
            .filter_map(|(_, variable)| variable.binding.as_ref())
            .collect::<Vec<_>>();
        if resources.len() != 1 || resources[0].group != 0 || resources[0].binding != 0 {
            return Err(error(
                origin,
                path,
                "only the generated @group(0) @binding(0) uniform block is allowed".to_owned(),
            ));
        }
        let module = Arc::new(ShaderModule {
            origin: origin.clone(),
            source: Arc::from(source),
        });
        cache.modules.insert(key, Arc::clone(&module));
        Ok(ShaderSpec { module, uniforms })
    }
}

#[cfg(feature = "render")]
mod wgsl {
    use super::{SourceUri, UiDocError, error};

    const PRELUDE_TAIL: &str = r#"}

@group(0) @binding(0)
var<uniform> kithara: KitharaUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}
"#;

    pub(super) fn combined(
        authored: &str,
        origin: &SourceUri,
        path: &str,
        fields: &[String],
    ) -> Result<String, UiDocError> {
        let mut source = String::from("struct KitharaUniforms {\n    viewport: vec4<f32>,\n");
        for name in fields {
            if name == "viewport" || !identifier(name) {
                return Err(error(
                    origin,
                    path,
                    format!("invalid or reserved uniform name {name:?}"),
                ));
            }
            source.push_str("    ");
            source.push_str(name);
            source.push_str(": vec4<f32>,\n");
        }
        source.push_str(PRELUDE_TAIL);
        source.push_str(authored);
        Ok(source)
    }

    fn identifier(name: &str) -> bool {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    }
}

fn error(origin: &SourceUri, path: &str, detail: String) -> UiDocError {
    UiDocError::Shader {
        origin: origin.clone(),
        path: path.to_owned(),
        detail,
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const FRAGMENT: &str = r#"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(position.xy / kithara.viewport.xy, kithara.level.x, 1.0);
}
"#;

    #[kithara::test]
    fn valid_authored_fragment_is_joined_to_the_owned_contract() {
        let mut cache = ShaderCache::default();
        let spec = compile(
            &mut cache,
            FRAGMENT,
            &SourceUri("shaders/meter.wgsl".to_owned()),
            "studio/meter",
            Vec::new(),
            &["level".to_owned()],
        )
        .unwrap_or_else(|cause| panic!("valid shader: {cause}"));

        assert!(spec.module.source.contains("var<uniform> kithara"));
        assert!(spec.module.source.contains("level: vec4<f32>"));
    }

    #[kithara::test]
    fn malformed_source_is_a_typed_load_error() {
        let mut cache = ShaderCache::default();
        let error = compile(
            &mut cache,
            "@fragment fn fs_main(",
            &SourceUri("shaders/broken.wgsl".to_owned()),
            "studio/broken",
            Vec::new(),
            &[],
        )
        .unwrap_err();

        assert!(matches!(error, UiDocError::Shader { .. }));
        assert!(error.to_string().contains("shaders/broken.wgsl"));
    }

    #[kithara::test]
    fn uniform_names_cannot_replace_the_standard_viewport() {
        let mut cache = ShaderCache::default();
        let error = compile(
            &mut cache,
            FRAGMENT,
            &SourceUri("shaders/meter.wgsl".to_owned()),
            "studio/meter",
            Vec::new(),
            &["viewport".to_owned()],
        )
        .unwrap_err();

        assert!(matches!(error, UiDocError::Shader { .. }));
    }
}

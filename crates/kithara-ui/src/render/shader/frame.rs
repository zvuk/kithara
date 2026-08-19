use kithara_platform::sync::Arc;
use num_traits::ToPrimitive as _;
use thiserror::Error;

use crate::{
    compile::CompiledUi,
    draw::ImageId,
    registry::ValueKind,
    render::{ReadValue, Reads},
    shader::ShaderSpec,
};

/// One immutable set of values for a document shader draw.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ShaderFrame {
    #[field(get, vis = "pub(crate)")]
    image: ImageId,
    #[field(get, vis = "pub(crate)", deref = false)]
    source: Arc<str>,
    values: Arc<[[f32; 4]]>,
}

/// A compiled shader binding that the live reader did not satisfy.
#[derive(Clone, Debug, Error, PartialEq)]
pub(crate) enum ShaderFrameError {
    #[error("shader {path:?} cannot read endpoint {endpoint:?} for uniform {uniform:?}")]
    Missing {
        path: String,
        uniform: String,
        endpoint: String,
    },
    #[error(
        "shader {path:?} uniform {uniform:?} expected {expected}, but endpoint {endpoint:?} returned {actual}"
    )]
    Kind {
        path: String,
        uniform: String,
        endpoint: String,
        expected: ValueKind,
        actual: &'static str,
    },
    #[error(
        "shader {path:?} endpoint {endpoint:?} returned a non-finite value for uniform {uniform:?}"
    )]
    NonFinite {
        path: String,
        uniform: String,
        endpoint: String,
    },
}

impl ShaderFrame {
    pub(crate) fn read(
        spec: &ShaderSpec,
        path: &str,
        reads: &dyn Reads,
        ui: &CompiledUi,
    ) -> Result<Self, ShaderFrameError> {
        let values = spec
            .uniforms()
            .iter()
            .map(|uniform| {
                let name = ui.resolve(uniform.name);
                let endpoint = ui.resolve(uniform.read.key);
                let value = reads
                    .get(endpoint)
                    .ok_or_else(|| ShaderFrameError::Missing {
                        path: path.to_owned(),
                        uniform: name.to_owned(),
                        endpoint: endpoint.to_owned(),
                    })?;
                value_for(uniform.kind, value)
                    .ok_or_else(|| ShaderFrameError::Kind {
                        path: path.to_owned(),
                        uniform: name.to_owned(),
                        endpoint: endpoint.to_owned(),
                        expected: uniform.kind,
                        actual: kind_of(value),
                    })?
                    .ok_or_else(|| ShaderFrameError::NonFinite {
                        path: path.to_owned(),
                        uniform: name.to_owned(),
                        endpoint: endpoint.to_owned(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            image: ImageId::new(path),
            source: spec.source(),
            values: Arc::from(values),
        })
    }

    pub(crate) fn uniform_byte_len(&self) -> usize {
        (self.values.len() + 1) * 16
    }

    pub(crate) fn write_uniforms(&self, viewport: [f32; 2], bytes: &mut Vec<u8>) {
        bytes.clear();
        bytes.reserve((self.values.len() + 1) * 16);
        write_vec4(bytes, [viewport[0], viewport[1], 0.0, 0.0]);
        for value in self.values.iter().copied() {
            write_vec4(bytes, value);
        }
    }
}

pub(crate) fn logical_extent(width: f32, height: f32) -> Option<[u32; 2]> {
    if [width, height]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }
    let size = [width.ceil().to_u32()?, height.ceil().to_u32()?];
    (size[0] > 0 && size[1] > 0).then_some(size)
}

fn value_for(kind: ValueKind, value: ReadValue<'_>) -> Option<Option<[f32; 4]>> {
    let packed = match (kind, value) {
        (ValueKind::Bool, ReadValue::Bool(value)) => {
            Some([f32::from(u8::from(value)), 0.0, 0.0, 0.0])
        }
        (ValueKind::Scalar, ReadValue::Scalar(value)) => value
            .to_f32()
            .filter(|value| value.is_finite())
            .map(|value| [value, 0.0, 0.0, 0.0]),
        (ValueKind::Stereo, ReadValue::Stereo(value)) => [value.l, value.r, value.volume]
            .iter()
            .all(|value| value.is_finite())
            .then_some([value.l, value.r, value.volume, 0.0]),
        _ => return None,
    };
    Some(packed)
}

const fn kind_of(value: ReadValue<'_>) -> &'static str {
    match value {
        ReadValue::Text(_) => "Text",
        ReadValue::Bool(_) => "Bool",
        ReadValue::Scalar(_) => "Scalar",
        ReadValue::Stereo(_) => "Stereo",
        ReadValue::PortalMap(_) => "PortalMap",
        ReadValue::Range(_) => "Range",
        ReadValue::Waveform(_) => "Waveform",
        ReadValue::Table(_) => "Table",
        ReadValue::Tree(_) => "Tree",
    }
}

fn write_vec4(bytes: &mut Vec<u8>, value: [f32; 4]) {
    for component in value {
        bytes.extend_from_slice(&component.to_ne_bytes());
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn viewport_precedes_authored_uniforms_in_stable_vec4_slots() {
        let frame = ShaderFrame {
            image: ImageId::new("shader/test"),
            source: Arc::from("shader"),
            values: Arc::from([[0.25, 0.5, 0.75, 0.0], [1.0, 0.0, 0.0, 0.0]]),
        };
        let mut bytes = Vec::new();

        frame.write_uniforms([320.0, 180.0], &mut bytes);

        let expected = [
            [320.0, 180.0, 0.0, 0.0],
            [0.25, 0.5, 0.75, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ]
        .into_iter()
        .flatten()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>();
        assert_eq!(bytes, expected);
        assert_eq!(frame.uniform_byte_len(), 48);
    }

    #[kithara::test]
    fn numeric_bindings_reject_non_finite_values() {
        assert_eq!(
            value_for(ValueKind::Scalar, ReadValue::Scalar(f64::NAN)),
            Some(None)
        );
        assert_eq!(
            value_for(
                ValueKind::Stereo,
                ReadValue::Stereo(crate::render::StereoLevels {
                    l: 0.0,
                    r: f32::INFINITY,
                    volume: 1.0,
                })
            ),
            Some(None)
        );
    }

    #[kithara::test]
    fn image_extent_rounds_logical_bounds_up_and_rejects_empty_boxes() {
        assert_eq!(logical_extent(15.1, 8.0), Some([16, 8]));
        assert_eq!(logical_extent(0.0, 8.0), None);
        assert_eq!(logical_extent(f32::NAN, 8.0), None);
    }
}

use std::collections::HashMap;

use rten::{Model as RtenGraph, NodeId, Value as RtenValue};
use rten_tensor::{AsView, Layout};

use crate::api::BeatError;

/// Simple f32 tensor with shape (row-major / C-order).
#[derive(Debug, Clone)]
pub(crate) struct Tensor {
    pub(crate) data: Vec<f32>,
    pub(crate) shape: Vec<usize>,
}

/// ONNX model loaded from bytes, run via the pure-Rust rten runtime.
pub(crate) struct RtenModel {
    input_map: HashMap<String, NodeId>,
    model: RtenGraph,
    output_ids: Vec<NodeId>,
    output_names: Vec<(NodeId, String)>,
}

/// Load a model from ONNX bytes; `name` tags load errors.
impl TryFrom<(&'static str, &[u8])> for RtenModel {
    type Error = BeatError;

    fn try_from((name, bytes): (&'static str, &[u8])) -> Result<Self, BeatError> {
        let model = RtenGraph::load(bytes.to_vec()).map_err(|e| BeatError::ModelLoad {
            model: name,
            reason: e.to_string(),
        })?;

        let input_map: HashMap<String, NodeId> = model
            .input_ids()
            .iter()
            .filter_map(|&id| {
                let info = model.node_info(id)?;
                let name = info.name()?;
                Some((name.to_string(), id))
            })
            .collect();

        let output_names: Vec<(NodeId, String)> = model
            .output_ids()
            .iter()
            .filter_map(|&id| {
                let info = model.node_info(id)?;
                let name = info.name()?;
                Some((id, name.to_string()))
            })
            .collect();

        let output_ids: Vec<NodeId> = model.output_ids().to_vec();

        Ok(Self {
            input_map,
            model,
            output_ids,
            output_names,
        })
    }
}

impl RtenModel {
    /// Run inference with named inputs, return named outputs.
    pub(crate) fn run(
        &mut self,
        inputs: &[(&str, &Tensor)],
    ) -> Result<HashMap<String, Tensor>, BeatError> {
        let rten_inputs: Vec<(NodeId, RtenValue)> = inputs
            .iter()
            .map(|(name, tensor)| {
                let node_id = self
                    .input_map
                    .get(*name)
                    .ok_or_else(|| BeatError::Inference {
                        reason: format!("rten: unknown input name '{name}'"),
                    })?;
                let value = RtenValue::from_shape(tensor.shape.as_slice(), tensor.data.clone())
                    .map_err(|e| BeatError::Inference {
                        reason: format!("rten: failed to create input tensor '{name}': {e}"),
                    })?;
                Ok((*node_id, value))
            })
            .collect::<Result<Vec<_>, BeatError>>()?;

        let inputs_with_views: Vec<_> = rten_inputs
            .iter()
            .map(|(id, val)| (*id, val.into()))
            .collect();

        let outputs = self
            .model
            .run(inputs_with_views, &self.output_ids, None)
            .map_err(|e| BeatError::Inference {
                reason: format!("rten: model run failed: {e}"),
            })?;

        let mut result = HashMap::new();
        for (&id, value) in self.output_ids.iter().zip(outputs) {
            let name = self
                .output_names
                .iter()
                .find(|(nid, _)| *nid == id)
                .map_or_else(|| format!("output_{id:?}"), |(_, n)| n.clone());

            let rten_tensor = value
                .into_tensor::<f32>()
                .ok_or_else(|| BeatError::Inference {
                    reason: format!("rten: output '{name}' is not f32"),
                })?;
            let shape: Vec<usize> = rten_tensor.shape().to_vec();
            let data: Vec<f32> = rten_tensor.to_vec();

            result.insert(name, Tensor { data, shape });
        }

        Ok(result)
    }
}

use std::collections::HashMap;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use rten::{Model as RtenGraph, NodeId, ValueOrView, ValueView};
use rten_tensor::{AsView, Layout};
use smallvec::SmallVec;

use crate::nn::api::BeatError;

/// Simple f32 tensor with shape (row-major / C-order).
#[derive(Debug)]
pub(crate) struct Tensor {
    pub(crate) data: SampleBuffer,
    pub(crate) shape: SmallVec<[usize; 4]>,
}

/// ONNX model loaded from bytes, run via the pure-Rust rten runtime.
pub(crate) struct RtenModel {
    input_map: HashMap<String, NodeId>,
    output_names: HashMap<NodeId, String>,
    model: RtenGraph,
    output_ids: Vec<NodeId>,
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

        let output_names: HashMap<NodeId, String> = model
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
            output_names,
            model,
            output_ids,
        })
    }
}

impl RtenModel {
    /// Run inference with named inputs, return named outputs.
    pub(crate) fn run<S>(
        &self,
        inputs: &[(&str, &Tensor)],
        pools: &PoolRegion<S>,
    ) -> Result<HashMap<String, Tensor>, BeatError>
    where
        S: HasPool<f32>,
    {
        let rten_inputs: Vec<(NodeId, ValueOrView<'_>)> = inputs
            .iter()
            .map(|(name, tensor)| {
                let node_id = self
                    .input_map
                    .get(*name)
                    .ok_or_else(|| BeatError::Inference {
                        reason: format!("rten: unknown input name '{name}'"),
                    })?;
                let value = ValueView::from_shape(tensor.shape.as_slice(), &tensor.data[..])
                    .map_err(|e| BeatError::Inference {
                        reason: format!("rten: failed to create input tensor '{name}': {e}"),
                    })?;
                Ok((*node_id, value.into()))
            })
            .collect::<Result<Vec<_>, BeatError>>()?;

        let outputs = self
            .model
            .run(rten_inputs, &self.output_ids, None)
            .map_err(|e| BeatError::Inference {
                reason: format!("rten: model run failed: {e}"),
            })?;

        let mut result = HashMap::with_capacity(self.output_ids.len());
        for (&id, value) in self.output_ids.iter().zip(outputs) {
            let name = self
                .output_names
                .get(&id)
                .map_or_else(|| format!("output_{id:?}"), Clone::clone);

            let rten_tensor = value
                .into_tensor::<f32>()
                .ok_or_else(|| BeatError::Inference {
                    reason: format!("rten: output '{name}' is not f32"),
                })?;
            let shape = SmallVec::from_slice(rten_tensor.shape());
            let values = rten_tensor.iter();
            let mut data = pools.get_with_len::<f32>(values.len())?;
            for (dst, src) in data.iter_mut().zip(values) {
                *dst = *src;
            }

            result.insert(name, Tensor { data, shape });
        }

        Ok(result)
    }
}

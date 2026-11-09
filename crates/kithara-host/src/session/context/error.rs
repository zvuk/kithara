use firewheel::{
    error::{ActivateError, AddEdgeError, RemoveNodeError, UpdateError},
    node::NodeError,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum GraphError {
    #[cfg(target_arch = "wasm32")]
    #[error("web output failed: {0}")]
    Device(String),
    #[error("session graph command capacity is occupied")]
    Full,
    #[error("{0}")]
    Node(NodeError),
    #[error(transparent)]
    Edge(#[from] AddEdgeError),
    #[error(transparent)]
    Remove(#[from] RemoveNodeError),
    #[error(transparent)]
    Update(#[from] UpdateError),
    #[error(transparent)]
    Activate(#[from] ActivateError),
}

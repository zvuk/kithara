//! Propagate-down cancellation built on a backend-selected node tree.
//! Cancellation drains local wakers before recursively firing weak children.

mod group;
mod node;
mod scope;
mod token;
mod wait;

pub use group::CancelGroup;
pub use scope::CancelScope;
pub use token::{CancelToken, CancelWakerGuard};
pub use wait::Cancelled;

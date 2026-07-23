//! Internal runtime abstractions shared by worker nodes and scheduler.

mod handle;
mod node;
pub(crate) mod observer;
mod ports;
mod scheduler;
pub(crate) mod wake;

pub(crate) use handle::{SchedulerCmd, SchedulerHandle, Slot, SlotId};
pub use node::ServiceClass;
pub(crate) use node::{AtomicServiceClass, Node, RtPolicy, TickResult};
pub(crate) use observer::{PassOutcome, PassReport, SchedulerEvent, SchedulerObserver};
pub(crate) use ports::{Inlet, Outlet, WakeSignal, connect};
pub(crate) use scheduler::Scheduler;
pub(crate) use wake::SchedulerWake;

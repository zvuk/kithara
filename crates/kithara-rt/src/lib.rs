//! Cooperative multi-node scheduler for real-time audio processing.

pub mod node;
pub mod observer;
pub mod ports;
pub mod schedule;
pub mod scheduler;
pub mod wake;

pub use node::{Node, ServiceClass, TickResult};
pub use observer::{NoopObserver, PassOutcome, SchedulerEvent, SchedulerObserver};
pub use ports::{Inlet, Outlet, WakeSignal, connect};
pub use schedule::{RoundRobin, Schedule, SlotMeta};
pub use scheduler::{Scheduler, SchedulerHandle, SlotId};
pub use wake::SchedulerWake;

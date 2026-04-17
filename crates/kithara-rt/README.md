# kithara-rt

Cooperative multi-node scheduler for real-time audio processing.

This crate provides a generic, lock-free runtime layer for executing multiple data processing nodes on a single OS thread. It is designed to be deterministic, allocation-free in the hot path, and extensible.

## Core Concepts

- **`Node`**: The fundamental unit of execution. A node performs a small quantum of work in its `tick()` method and returns a `TickResult` (`Progress`, `Waiting`, or `Done`).
- **`Scheduler`**: The execution engine. It runs on a dedicated OS thread, iterating over registered nodes and calling their `tick()` methods. It handles panic isolation, graceful shutdown, and dynamic node registration.
- **`Schedule`**: A pluggable strategy for determining the order in which nodes are ticked (e.g., `RoundRobin`).
- **`SchedulerObserver`**: A pluggable mechanism for observing the scheduler's behavior (e.g., for watchdog integration or metrics).
- **Ports (`Inlet` / `Outlet`)**: Lock-free SPSC ring buffers for passing data between nodes or between a node and an external thread. `Outlet` carries a single-slot overflow that absorbs one backpressure miss per tick — producers that emit at most one item per tick can treat `try_push` as infallible and use `flush()` at the start of the next tick to forward the parked item once the consumer drains the ring.

## Design Principles

1. **Static Dispatch**: The scheduler is generic over the node type (`Scheduler<N: Node>`). To support heterogeneous nodes, wrap them in an `enum` that implements `Node`. This avoids the overhead of `Box<dyn Node>` in the hot path.
2. **Cooperative Multitasking**: Nodes must not block the thread. If a node cannot make progress (e.g., due to backpressure or missing input), it should return `TickResult::Waiting`.
3. **Backpressure**: Handled naturally via `Outlet::try_push`. The outlet absorbs one push past the ring boundary into its overflow slot; subsequent ticks call `Outlet::flush()` to forward it once the consumer drains the ring. If both ring and overflow are saturated, `try_push` returns `Err(item)` and the node should return `TickResult::Waiting`. The scheduler will sleep and retry later.
4. **Panic Isolation**: If a node panics during `tick()`, the scheduler catches the panic, marks the node as `Done`, and continues executing other nodes.

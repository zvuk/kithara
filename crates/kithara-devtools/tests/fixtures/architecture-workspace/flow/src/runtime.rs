use std::{
    sync::{Arc, mpsc},
    thread,
};

#[derive(Clone, Copy)]
pub struct ResourceId(pub u64);

struct SharedState {
    id: ResourceId,
}

trait Work {
    fn run(&self, id: ResourceId);
}

struct Primary;

impl Primary {
    fn new() -> Self {
        Self
    }
}

impl Work for Primary {
    fn run(&self, id: ResourceId) {
        recurse(id, 1);
    }
}

pub fn start(id: ResourceId) {
    let id = local(id);
    let state = Arc::new(SharedState { id });
    let state_for_task = Arc::clone(&state);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || worker(state_for_task, sender));
    let _ = receiver.recv();
    let primary = Primary::new();
    dispatch(&primary, state.id);
}

fn local(id: ResourceId) -> ResourceId {
    id
}

fn worker(state: Arc<SharedState>, sender: mpsc::Sender<ResourceId>) {
    let _ = sender.send(state.id);
}

fn dispatch(worker: &dyn Work, id: ResourceId) {
    worker.run(id);
}

fn recurse(id: ResourceId, remaining: u8) {
    if remaining > 0 {
        recurse(id, remaining - 1);
    }
}

fn invoke(callback: fn(ResourceId), id: ResourceId) {
    callback(id);
}

#[cfg(test)]
mod tests {
    struct InlineNoise;

    fn inline_helper() {}
}

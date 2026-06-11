//! `FLASH_AMBIENT` spawn propagation (B5): the platform spawn wrappers carry the
//! per-test ambient gate into spawned children (threads and async tasks), since
//! thread-locals do not cross `spawn`. Gated on `flash` (native only).
#![cfg(all(not(target_arch = "wasm32"), feature = "flash"))]

use std::sync::mpsc;

use kithara_platform::{
    flash::{ambient_scope, ambient_snapshot},
    thread::spawn_named,
    tokio,
};

#[test]
fn spawn_named_propagates_ambient() {
    let _a = ambient_scope(true);
    let (tx, rx) = mpsc::channel();
    spawn_named("probe", move || {
        tx.send(ambient_snapshot())
            .expect("BUG: receiver must outlive the probe thread");
    });
    assert!(
        rx.recv().expect("probe thread must report before exiting"),
        "child thread must see ambient=true"
    );
}

#[test]
fn spawn_named_without_ambient_is_false() {
    let (tx, rx) = mpsc::channel();
    spawn_named("probe2", move || {
        tx.send(ambient_snapshot())
            .expect("BUG: receiver must outlive the probe thread");
    });
    assert!(
        !rx.recv().expect("probe thread must report before exiting"),
        "child without ambient sees false"
    );
}

#[test]
fn spawn_task_propagates_ambient() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime must build");
    let seen = rt.block_on(async {
        let _a = ambient_scope(true);
        tokio::task::spawn(async { ambient_snapshot() })
            .await
            .expect("spawned task must join cleanly")
    });
    assert!(seen, "spawned async task must see ambient=true");
}

#[test]
fn spawn_task_without_ambient_is_false() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime must build");
    let seen = rt.block_on(async {
        tokio::task::spawn(async { ambient_snapshot() })
            .await
            .expect("spawned task must join cleanly")
    });
    assert!(!seen, "spawned async task without ambient sees false");
}

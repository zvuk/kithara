#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::process::Command;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, permit_alloc};
use kithara_test_utils::kithara;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[kithara::test(native)]
fn nested_permit_and_foreign_thread_are_allowed() {
    let prepared = vec![1_u8; 64];
    assert_no_alloc(|| {
        permit_alloc(|| {
            std::hint::black_box(prepared.clone());
        });
    });
    let worker = std::thread::spawn(|| {
        std::hint::black_box(vec![0_u8; 144]);
    });
    assert_no_alloc(|| {
        std::hint::black_box(prepared.len());
    });
    worker.join().expect("foreign allocation worker");
}

#[kithara::test(native)]
fn forbidden_allocation_aborts_optimized_artifact() {
    if std::env::var_os("KITHARA_GUARD_ALLOCATION_CONTROL").is_some() {
        assert_no_alloc(|| {
            std::hint::black_box(vec![0_u8; 64]);
        });
        return;
    }
    assert!(abort_control(
        "forbidden_allocation_aborts_optimized_artifact",
        "KITHARA_GUARD_ALLOCATION_CONTROL"
    ));
}

#[kithara::test(native)]
fn forbidden_free_aborts_optimized_artifact() {
    if std::env::var_os("KITHARA_GUARD_FREE_CONTROL").is_some() {
        let allocated = vec![0_u8; 64];
        assert_no_alloc(|| drop(allocated));
        return;
    }
    assert!(abort_control(
        "forbidden_free_aborts_optimized_artifact",
        "KITHARA_GUARD_FREE_CONTROL"
    ));
}

fn abort_control(test: &str, variable: &str) -> bool {
    let executable = std::env::current_exe().expect("guard control executable");
    let output = Command::new(executable)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(variable, "1")
        .output()
        .expect("run isolated guard control");
    !output.status.success()
}

use kithara_platform::CancelToken;
use kithara_storage::{
    Active, DriverIo, MemResource, MmapOptions, MmapResource, Resource, ResourceRead, StorageError,
};
use kithara_test_utils::{
    TestTempDir, bufpool::pools, cancel_token, cancel_token_cancelled, kithara, temp_dir,
};

#[kithara::test]
#[case::failed_mem(false, Some("broken"), false)]
#[case::failed_mmap(true, Some("broken"), false)]
#[case::cancelled_mem(false, None, false)]
#[case::cancelled_mmap(true, None, false)]
#[case::wait_cancelled_mem(false, None, true)]
#[case::wait_cancelled_mmap(true, None, true)]
fn a_written_range_answers_its_error_first(
    #[case] mmap: bool,
    #[case] failure: Option<&str>,
    #[case] cancel_wait: bool,
    temp_dir: TestTempDir,
    cancel_token: CancelToken,
    cancel_token_cancelled: CancelToken,
) {
    let wait_cancel = cancel_wait.then_some(&cancel_token_cancelled);
    if mmap {
        let resource: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions::for_path(temp_dir.path().join("available.dat")).build(),
        )
        .expect("open the mmap resource");
        assert_range_error(resource, failure, &cancel_token, wait_cancel);
    } else {
        let resource = MemResource::new(cancel_token.clone(), pools().get::<u8>());
        assert_range_error(resource, failure, &cancel_token, wait_cancel);
    }
}

fn assert_range_error<D: DriverIo>(
    resource: Resource<Active, D>,
    failure: Option<&str>,
    cancel: &CancelToken,
    wait_cancel: Option<&CancelToken>,
) {
    resource
        .write_at(0, b"data")
        .expect("write the covered range");
    let reader = resource.reader();
    if let Some(reason) = failure {
        resource.fail(reason.to_owned());
        assert!(matches!(
            reader.wait_range(0..4),
            Err(StorageError::Failed(_))
        ));
        return;
    }
    let result = wait_cancel.map_or_else(
        || {
            cancel.cancel();
            reader.wait_range(0..4)
        },
        |wait_cancel| reader.wait_range_with_cancel(0..4, wait_cancel),
    );
    assert!(matches!(result, Err(StorageError::Cancelled)));
}

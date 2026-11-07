use std::future::Future;

use kithara_platform::{CancelGroup, CancelToken, maybe_send::MaybeSend};
use kithara_worker::Worker;

use crate::LoadRefusal;

pub(in crate::worker) type Preparation = ();

pub(in crate::worker) fn new(
    _worker: &Worker,
    _id: u64,
    _cancel: Option<CancelGroup>,
) -> Preparation {
}

pub(in crate::worker) async fn execute<F, P, R>(
    _preparation: &Preparation,
    _cancel: Option<CancelToken>,
    factory: F,
) -> Result<R, LoadRefusal>
where
    F: FnOnce() -> P + MaybeSend + 'static,
    P: Future<Output = Result<R, LoadRefusal>> + 'static,
    R: MaybeSend + 'static,
{
    factory().await
}

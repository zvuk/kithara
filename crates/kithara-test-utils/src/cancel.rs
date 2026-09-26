//! Cancel-token fixtures: a test is the root of its own cancel tree.

use kithara_platform::CancelToken;

use crate::kithara;

/// A token that is never cancelled unless the test cancels it.
#[must_use]
#[kithara::fixture]
pub fn cancel_token() -> CancelToken {
    CancelToken::never()
}

/// A token that is already cancelled.
#[must_use]
#[kithara::fixture]
pub fn cancel_token_cancelled() -> CancelToken {
    let token = CancelToken::never();
    token.cancel();
    token
}

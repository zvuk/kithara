# kithara-net — Context

Detailed contracts and invariants for the kithara-net crate; the README is the overview.

## Decorators

`TimeoutNet<N>` wraps all methods with `tokio::time::timeout` and is exported in the public API. A retry decorator with exponential backoff (retries on 5xx, 429, 408, timeouts; does not retry on other 4xx) is also available, but only via the `NetExt` builder methods — the wrapper type itself is not part of the public surface.

Decorators compose via the `NetExt` extension trait:
```rust
use kithara_net::{HttpClient, Net, NetExt, NetOptions, RetryPolicy};
use std::time::Duration;
use kithara_platform::CancelToken;

let client = HttpClient::new(NetOptions::default(), CancelToken::never())
    .with_retry(RetryPolicy::default(), CancelToken::never())
    .with_timeout(Duration::from_secs(30));
```

## Timeout Behavior

Two independent limits in `NetOptions`, applied to **all** methods (`get_bytes`,
`head`, `get_range`, `stream`):

- `inactivity_timeout` (default 30s) — max gap between reads (reqwest
  `read_timeout`); guards against stalled connections, not total duration.
- `total_timeout` (default 120s) — hard cap on request lifetime. Set to `None`
  to allow indefinite streaming as long as data keeps flowing.

The `TimeoutNet` decorator can wrap any `Net` with an additional
`tokio::time::timeout` over the whole call.

## Trait Bridges

- `&RangeSpec` → `String` (`Display`) — HTTP Range header rendering
- `HashMap<String, String>` → `Headers` (`From`) — build header set from a map
- `Compression` → `Vec<ClientBuilderMod>` (`From`) — map compression flags to reqwest builder mods
- `ReqwestError` → `NetError` (`From`) — wrap transport errors into typed `NetError`

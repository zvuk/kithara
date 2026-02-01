<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

# kithara-net

HTTP networking with retry, timeout, and streaming support. Provides the `Net` trait for HTTP operations and `HttpClient` as the default reqwest-based implementation. Includes `TimeoutNet` decorator and `MockNet` for testing.

## Usage

```rust
use kithara_net::{HttpClient, Net, NetOptions};

let client = HttpClient::new()?;
let bytes = client.get_bytes(url, &NetOptions::default()).await?;
let stream = client.stream(url, &NetOptions::default()).await?;
```

## Integration

Used by `kithara-file` and `kithara-hls` for all HTTP operations. `MockNet` (behind `test-utils` feature) enables deterministic testing without network access.

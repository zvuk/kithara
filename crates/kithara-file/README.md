<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-file

Progressive file download and playback for single-file media (MP3, AAC, etc.). Implements `StreamType` for use with `Stream<File>`, providing HTTP download with disk caching, seeking, and progress events.

## Usage

```rust
use kithara_stream::Stream;
use kithara_file::{File, FileConfig};

let config = FileConfig::new(url);
let stream = Stream::<File>::new(config).await?;
```

## Integration

Depends on `kithara-net` for HTTP and `kithara-assets` for caching. Composes with `kithara-decode` as `Decoder<Stream<File>>` for full decode pipeline.

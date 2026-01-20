//! Integration tests for HLS events and broadcast channel.

use kithara_hls::HlsEvent;
use tokio::sync::broadcast;

#[test]
fn test_broadcast_channel() {
    let (tx, mut rx) = broadcast::channel::<HlsEvent>(32);
    let _ = tx.send(HlsEvent::EndOfStream);

    let event = rx.try_recv().ok();
    assert!(matches!(event, Some(HlsEvent::EndOfStream)));
}

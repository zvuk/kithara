mod fixture;
use fixture::*;
use kithara_hls::abr::{AbrConfig, AbrController};
use kithara_hls::events::EventEmitter;
use kithara_hls::fetch::FetchManager;
use kithara_hls::keys::KeyManager;
use kithara_hls::playlist::PlaylistManager;
use kithara_hls::{HlsOptions, HlsResult};
use tokio::sync::mpsc;

// #[tokio::test]
// async fn driver_initialization() -> HlsResult<()> {
//     let server = TestServer::new().await;
//     let (cache, net) = create_test_cache_and_net();
//
//     let master_url = server.url("/master.m3u8")?;
//     let options = HlsOptions::default();
//
//     let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
//     let fetch_manager = FetchManager::new(cache.clone(), net.clone());
//     let key_manager = KeyManager::new(cache.clone(), net.clone(), None, None, None);
//     let abr_config = AbrConfig::default();
//     let abr_controller = AbrController::new(abr_config, None, 0);
//     let event_emitter = EventEmitter::new();
//
//     let (cmd_sender, cmd_receiver) = mpsc::channel(16);
//     let (bytes_sender, _bytes_receiver) = mpsc::channel(100);
//
//     let driver = HlsDriver::new(
//         master_url,
//         options,
//         playlist_manager,
//         fetch_manager,
//         key_manager,
//         abr_controller,
//         event_emitter,
//         cmd_receiver,
//         bytes_sender,
//     );
//
//     // Verify driver is created successfully
//     assert!(matches!(
//         driver.state,
//         kithara_hls::driver::DriverState::Starting
//     ));
//
//     Ok(())
// }
//
// #[tokio::test]
// async fn handle_stop_command() -> HlsResult<()> {
//     let server = TestServer::new().await;
//     let (cache, net) = create_test_cache_and_net();
//
//     let master_url = server.url("/master.m3u8")?;
//     let options = HlsOptions::default();
//
//     let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
//     let fetch_manager = FetchManager::new(cache.clone(), net.clone());
//     let key_manager = KeyManager::new(cache.clone(), net.clone(), None, None, None);
//     let abr_config = AbrConfig::default();
//     let abr_controller = AbrController::new(abr_config, None, 0);
//     let event_emitter = EventEmitter::new();
//
//     let (cmd_sender, cmd_receiver) = mpsc::channel(16);
//     let (bytes_sender, bytes_receiver) = mpsc::channel(100);
//
//     let driver = HlsDriver::new(
//         master_url,
//         options,
//         playlist_manager,
//         fetch_manager,
//         key_manager,
//         abr_controller,
//         event_emitter,
//         cmd_receiver,
//         bytes_sender,
//     );
//
//     // Send stop command
//     cmd_sender
//         .send(kithara_hls::HlsCommand::Stop)
//         .await
//         .unwrap();
//
//     // Note: In real test, we would verify driver stops
//     // For now, just verify the command is sent successfully
//
//     Ok(())
// }

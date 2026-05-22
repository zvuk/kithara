/// Compile-time switch between the two demo view-model variants
/// shipped side-by-side in `Sources/`:
///
/// - ``PlayerViewModelCombine`` — drives `@Published` state from
///   `KitharaPlayer.eventPublisher` via Combine.
/// - ``PlayerViewModelRx`` — same `@Published` UI surface but every
///   subscription flows through `KitharaRx` + `RxSwift`, no
///   `import Combine` inside the file.
///
/// The `KitharaDemoRx_iOS` target compiles with `-D KITHARA_RX` so
/// `PlayerView`'s `@StateObject` resolves to the Rx variant; the
/// default `KitharaDemo_iOS` target keeps the Combine baseline.
#if KITHARA_RX
typealias PlayerViewModel = PlayerViewModelRx
#else
typealias PlayerViewModel = PlayerViewModelCombine
#endif

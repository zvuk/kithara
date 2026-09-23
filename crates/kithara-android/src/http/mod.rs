//! The host application's HTTP transport, `com.kithara.net.HttpTransport`,
//! as the `HostTransport` Kithara's HTTP client runs every request through.
//!
//! The application hands its transport over once through
//! `NativeHttpTransport.install`, which installs it into `kithara-net`; the
//! transport reports each call back through the `NativeHttpCallback` natives
//! exported here.

mod callback;
mod exports;
mod transport;

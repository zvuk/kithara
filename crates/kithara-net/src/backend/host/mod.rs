mod call;
mod client;
mod exchange;
mod protocol;
mod slot;
#[cfg(test)]
mod tests;
mod transport;

pub use client::HostNet as HttpClient;
pub use slot::{AlreadyInstalled, install};
pub use transport::{
    HostBuffer, HostCall, HostEvents, HostFailure, HostMethod, HostRequest, HostRequestBody,
    HostTransport,
};

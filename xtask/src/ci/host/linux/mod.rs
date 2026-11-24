mod cleanup;
pub(crate) mod command;
mod compose;
mod container;
mod cpu;
mod deployment;
mod firewall;
mod gitlab;
mod permissions;
mod profile;
mod registration;
mod services;
mod system;
mod windows;

pub(crate) use command::run;

#![forbid(unsafe_code)]

mod account;
mod client;
mod config;
mod consts;
mod error;
mod job;
mod media;
mod model;
mod page;
mod streaming;
mod ui;

pub use crate::{account::Opener, client::Client, config::Config, ui::Source};
pub(crate) use crate::{
    account::{Account, Command},
    error::{Error, GraphQlError},
    media::MediaTrack,
    model::{Playlist, PlaylistId, Track, TrackId},
    page::TrackPage,
};

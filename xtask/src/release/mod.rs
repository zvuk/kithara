//! The release flow. CI builds every artifact once, then publishing runs the
//! channel's steps over those bytes: the tag step stamps the Swift manifest
//! and the changelog on a commit beside the branch and tags it on both
//! remotes; the release step publishes that tag with its changelog section on
//! GitHub and `GitLab`; the Pages step lays the demo, the documentation, the
//! archives, and the crates out as the project site. The nightly channel
//! replaces one rolling release with what the branch built today.

mod app;
mod artifact;
mod changelog;
mod command;
mod git;
mod github;
mod gitlab;
mod notes;
mod site;
mod tag;

pub(crate) use artifact::sha256;
pub(crate) use command::{
    ReleaseArgs, publish_nightly, publish_pages, publish_release, run, tag_release,
};
pub(crate) use site::hosting_base;

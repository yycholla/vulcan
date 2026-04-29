//! Long-lived `vulcan daemon` process + Unix-socket client surface
//! (YYC-266). This module is gated by the `daemon` feature so embedders /
//! minimal builds can omit the daemon machinery entirely.
//!
//! Slice 0 lays down the wire protocol (this submodule), the
//! length-delimited frame I/O over any [`tokio::io::AsyncRead`] /
//! [`tokio::io::AsyncWrite`], and the daemon skeleton.

pub mod cli;
pub mod config_watch;
pub mod dispatch;
pub mod handlers;
pub mod lifecycle;
pub mod protocol;
pub mod server;
pub mod session;
pub mod state;

#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod protocol_tests;

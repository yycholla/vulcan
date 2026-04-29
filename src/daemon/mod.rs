//! Long-lived `vulcan daemon` process + Unix-socket client surface
//! (YYC-266). This module is gated by the `daemon` feature so embedders /
//! minimal builds can omit the daemon machinery entirely.
//!
//! Slice 0 lays down the wire protocol (this submodule) and the daemon
//! skeleton. Frame I/O lands in Task 0.3.

pub mod protocol;

#[cfg(test)]
mod protocol_tests;

//! Method handlers, grouped by subject prefix (daemon, agent, prompt,
//! cortex, session, approval). Slice 0 only ships `daemon_ops`; the
//! rest land in later slices.

pub mod cortex;
pub mod daemon_ops;

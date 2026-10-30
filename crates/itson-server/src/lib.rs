//! The It's On server, without its network and database adapters.
//!
//! Everything here is plain Rust driven by the caller: the HTTP and QUIC
//! front ends and the SQLite store plug in from the outside. That keeps the
//! rules testable with fictional data and lets the storage change later.
//!
//! Layering:
//!
//! - [`store`]: what is persisted and the [`store::Store`] trait, with an
//!   in-memory implementation.
//! - [`password`] and [`token`]: credential handling.
//! - [`service`]: the operations a server offers, with permission checks.
//! - [`gateway`]: the per-connection protocol state machine and the event
//!   log that makes `resume` possible.

pub mod gateway;
pub mod limits;
pub mod password;
pub mod service;
pub mod store;
pub mod token;

pub use service::{Server, ServiceError};

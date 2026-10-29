//! The It's On server core, SQLite store, and HTTP/WebSocket adapter.
//!
//! The rules remain independent of transport. The HTTP adapter drives the
//! core through a database worker, keeping blocking work off async threads.
//!
//! Layering:
//!
//! - [`store`]: what is persisted and the [`store::Store`] trait, with an
//!   in-memory implementation.
//! - [`sqlite`]: the persistent store, with versioned schema migrations.
//! - [`http`]: HTTP routes, WebSocket transport and database worker.
//! - [`password`] and [`token`]: credential handling.
//! - [`service`]: the operations a server offers, with permission checks.
//! - [`gateway`]: the per-connection protocol state machine and the event
//!   log that makes `resume` possible.

pub mod gateway;
pub mod http;
pub mod password;
pub mod service;
pub mod sqlite;
pub mod store;
pub mod token;

pub use service::{Server, ServiceError};

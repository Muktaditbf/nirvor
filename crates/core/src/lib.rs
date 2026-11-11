//! nirvor core: the DNS listeners and query pipeline.
//!
//! Pipeline: parse → validate → cache → upstream pool (coalesced) → serve-stale fallback → fit to the client's UDP size.

pub mod config;
pub mod handler;
pub mod metrics;
pub mod mode;
pub mod querylog;
pub mod server;

pub use config::Config;
pub use server::Server;

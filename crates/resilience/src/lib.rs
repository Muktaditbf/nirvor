//! Local resilience: root zone copy (RFC 8806), cache snapshots and iterative resolution.

pub mod recursor;
pub mod rootzone;
pub mod snapshot;
pub mod zonefile;

pub use recursor::Recursor;
pub use rootzone::{RootManager, RootZoneConfig};

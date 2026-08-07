//! The operational layer: replicating the log to a NAS/remote copy over HTTP
//! (`transport`), draining a replica's captured writes back into the
//! authority (`drain`), installing THOR's hooks into an agent's settings file
//! (`install`), and a plain-language health check over both plus the fact
//! store (`health`).

pub mod drain;
pub mod health;
pub mod install;
pub mod transport;

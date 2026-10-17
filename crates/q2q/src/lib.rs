//! Client SDK for publishing and downloading content on a Q2Q network.
//!
//! The SDK prepares a persistent identity, publishes local files and downloads
//! verified blocks through authenticated WebRTC peer sessions.

mod admission;
mod block_queue;
mod cdn;
mod control;
mod destination;
mod discovery;
mod download;
mod download_policy;
mod download_store;
mod error;
mod identity;
mod online;
mod peer;
mod peer_identity;
mod peer_transfer;
mod progress;
mod punch_transport;
mod reader;
mod scheduler;
mod signal_router;
mod source_file;
mod task_scope;
pub mod transfer;
mod upload;
mod upload_cache;
mod upload_progress;
mod upload_rate;
mod upload_router;
mod upload_session;
mod upload_source;
mod webrtc_transport;

pub mod client;
pub mod publication;

/// The exact wire shapes used by both the SDK and the services.
pub mod protocol {
  pub use q2q_core::peer_wire as peer;
  pub use q2q_punch::message as punch;
  pub use q2q_track::wire as track;
}

#[cfg(test)]
#[path = "../tests/http.rs"]
mod test_http;

#[cfg(test)]
#[path = "../tests/destination.rs"]
mod test_destination;

#[cfg(test)]
#[path = "../tests/upload_cache_bench.rs"]
mod upload_cache_bench;

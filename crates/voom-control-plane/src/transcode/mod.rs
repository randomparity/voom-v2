//! Byte-free transcode-video planning helpers: profile resolution for
//! policies and the output naming shared with envelope rendering (ADR 0075).
//!
//! The bundled control-plane transcode execute path was removed in the T8
//! sweep: transcode-video tickets execute exclusively through their storage
//! owner's agent via `media_dispatch` envelopes.

pub(crate) mod resolve;
pub(crate) mod stage;

pub use resolve::ResolvedProfile;

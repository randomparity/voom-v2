//! Byte-free remux selection derivation shared with envelope rendering
//! (ADR 0075).
//!
//! The bundled control-plane remux execute path was removed in the T8 sweep:
//! remux tickets execute exclusively through their storage owner's agent via
//! `media_dispatch` envelopes.

pub(crate) mod selection;

//! `voom library` / `voom library root` CRUD command surface (T11).

pub mod library;
pub mod root;

/// Envelope `command` field for every library/root subcommand.
pub(crate) const COMMAND: &str = "library";

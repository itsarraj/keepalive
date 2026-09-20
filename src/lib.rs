//! Core library for `keepalive` — see `src/main.rs` for the CLI, and
//! this crate's README for what it does and why.

pub mod backoff;
pub mod duration;
pub mod logfile;
pub mod restart;
pub mod supervisor;

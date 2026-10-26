pub mod adapters;
pub(crate) mod adoption;
mod atomic_write;
pub(crate) mod audit_plan;
pub mod cli;
pub mod command;
pub mod config;
pub mod consistency;
pub mod context;
pub mod coverage;
mod diagnostics;
mod effects;
pub mod error;
pub mod footprint;
pub mod git;
pub mod landed;
pub mod logging;
mod owned;
// Path encoding lives in the canonical crate: these encoders sit behind
// `#[serde(with = ...)]` on sealed structs, so the bytes they produce are the
// bytes that get digested. Re-exported under the old name so call sites — and
// the `serde(with = "crate::path_codec::...")` attributes, which resolve a
// path, not a module — are unchanged.
pub(crate) use sauma_canonical::path_codec;
pub mod refresh;
pub mod relevance;
pub mod render;
pub mod repo;
pub mod sauma;
pub mod semantic;
pub mod session;
pub mod sessions;
pub mod snapshot;
pub mod snippet;
pub mod validate;
pub mod watch;

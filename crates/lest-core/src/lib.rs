//! Lest core: the flow format, validation, the runner and run reports.

/// The version this binary reports: `LEST_VERSION` at build time (release
/// builds set it from the tag), else the crate version.
pub const VERSION: &str = match option_env!("LEST_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub mod affected;
pub mod ai;
pub mod browser;
pub mod bundle;
pub mod catalog;
pub mod docs;
pub mod event;
pub mod expr;
pub mod headline;
pub mod media;
pub mod notify;
pub mod paths;
pub mod process;
pub mod project;
pub mod report;
pub mod runner;
pub mod secrets;
pub mod services;
pub mod spec;
pub mod store;
pub mod tools;
pub mod validate;

//! DPI-bypass (`winws`) discovery and control helpers.
//!
//! The strategy catalog is generated from Flowseal's Windows zapret bundle
//! (`tools/gen-strategies.ps1`). This crate only renders command lines, probes
//! connectivity and talks to the helper service; it never loads a driver and
//! never needs elevation by itself.

pub mod catalog;
pub mod probe;
pub mod render;
pub mod scope;
pub mod service;

pub use catalog::{Catalog, CatalogError, Strategy};
pub use probe::Probe;
pub use render::{GameFilter, Paths, render};
pub use scope::Scope;
pub use service::{Service, ServiceState};

/// Service name used by the installer for the privileged `winws` host.
pub const SERVICE_NAME: &str = "serein-zapret";

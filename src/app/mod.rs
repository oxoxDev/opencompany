pub mod config;
pub mod doctor;
pub mod journal;
mod types;

pub use config::{BrainMode, ConfigProvenance, RuntimeConfig, resolve};
pub use doctor::{DoctorReport, report as doctor_report};
pub use types::{AppConfig, AppSpec, AppState, canonical_tenant, namespace_company_id};

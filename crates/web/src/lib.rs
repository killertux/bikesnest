//! BikesNest web crate: axum routing, handlers, Askama templates.
pub mod profile;
pub use profile::{CollaborationProposalVm, CollaborationRevisionVm};

pub mod assets;
pub mod auth;
pub mod client_ip;
pub mod htmx;
pub mod i18n;
pub mod markdown;
pub mod observability;
mod pages;
pub mod routes;
pub mod security;
pub mod state;
pub mod view;
pub mod wiring;

pub use pages::*;

pub use wiring::{RouterDeps, app_router, app_router_with};

#[cfg(test)]
mod lib_tests;

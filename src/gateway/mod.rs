//! Gateway configuration is deliberately independent from coding-agent CLIs.
//!
//! This module owns non-secret gateway metadata, validation, persistence, and
//! credential storage. CLI-specific launch translation belongs in adapters,
//! not here.

// The next stacked change connects these types to the adapter registry. Keep
// this foundation independently reviewable without weakening warnings in the
// rest of the application.
#![allow(dead_code)]

mod credentials;
mod model;
mod redact;
mod repository;

pub(crate) use credentials::{Credential, CredentialStore};
#[cfg(test)]
pub(crate) use credentials::{CredentialBackend, CredentialStoreError};
pub(crate) use model::{
    AuthenticationMode, Gateway, GatewayCatalog, GatewayFeature, GatewayProtocol, ValidationError,
};
pub(crate) use redact::redact;

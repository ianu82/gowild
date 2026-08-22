//! Gateway configuration is deliberately independent from coding-agent CLIs.
//!
//! This module owns non-secret gateway metadata, validation, persistence, and
//! credential storage. CLI-specific launch translation belongs in adapters,
//! not here.

// Custom-gateway forms land in the next stack. Keep the model fields they will
// expose available without weakening warnings in the rest of the application.
#![allow(dead_code)]

mod credentials;
mod model;
mod redact;
mod repository;
mod service;

#[cfg(test)]
pub(crate) use credentials::CredentialStoreError;
pub(crate) use credentials::{
    Credential, CredentialBackend, CredentialRemoval, CredentialStore, SystemCredentialStore,
};
pub(crate) use model::{
    AuthenticationMode, ConnectionStatus, Gateway, GatewayCatalog, GatewayFeature, GatewayPreset,
    GatewayProtocol, ValidationError, MINDSHUB_RESPONSES_BASE_URL,
};
#[cfg(test)]
pub(crate) use model::{CachedModel, ConnectionTest, ProtocolTest};
pub(crate) use redact::redact;
pub(crate) use repository::GatewayRepository;
pub(crate) use service::{GatewayInspection, GatewayTester, GatewayTesterError};

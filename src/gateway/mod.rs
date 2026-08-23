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

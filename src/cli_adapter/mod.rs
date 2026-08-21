//! Coding-CLI launch planning.
//!
//! Adapters translate a resolved gateway into one CLI's argv and child
//! environment. They do not read configuration or credentials themselves.

// Adapter launches are intentionally staged ahead of the TUI settings surface.
// Keep the launch layer warning-clean while that integration is built.
#![allow(dead_code, unused_imports)]

mod claude;
mod codex;
mod launch;
mod registry;
mod resolver;

pub(crate) use claude::ClaudeAdapter;
pub(crate) use codex::CodexAdapter;
pub(crate) use launch::{
    AdapterError, ChildEnvironment, ChildEnvironmentValue, CliAdapter, CodingCli,
    ExecutableLocator, LaunchError, LaunchMode, LaunchPlanner, LaunchRequest, LaunchSpec,
    LaunchSpecError, PathExecutableLocator,
};
pub(crate) use registry::{AdapterRegistry, RegistryError};
pub(crate) use resolver::{
    Environment, GatewayResolutionError, GatewayResolver, OsEnvironment, ResolvedGateway,
    ENV_API_KEY, ENV_GATEWAY, ENV_MESSAGES_BASE_URL, ENV_MODEL, ENV_RESPONSES_BASE_URL,
};

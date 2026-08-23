//! Coding-CLI launch planning.
//!
//! Adapters translate a resolved gateway into one CLI's argv and child
//! environment. They do not read configuration or credentials themselves.

// Concrete Codex and Claude adapters are intentionally added in the next two
// stacked changes. This infrastructure is compiled and tested independently.
#![allow(dead_code, unused_imports)]

mod launch;
mod registry;
mod resolver;

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

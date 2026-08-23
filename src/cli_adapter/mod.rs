//! Coding-CLI launch planning.
//!
//! Adapters translate a resolved gateway into one CLI's argv and child
//! environment. They do not read configuration or credentials themselves.

mod anthropic_bridge;
mod bridge_http;
mod claude;
mod codex;
mod launch;
mod registry;
mod resolver;
mod responses_bridge;

pub(crate) use anthropic_bridge::AnthropicBridge;
pub(crate) use claude::ClaudeAdapter;
pub(crate) use codex::CodexAdapter;
pub(crate) use launch::{
    AdapterError, ChildEnvironment, CliAdapter, CodingCli, ExecutableLocator, LaunchMode,
    LaunchPlanner, LaunchRequest, LaunchSpec, PathExecutableLocator,
};
pub(crate) use registry::AdapterRegistry;
pub(crate) use resolver::{Environment, GatewayResolutionError, GatewayResolver, ResolvedGateway};
pub(crate) use responses_bridge::ResponsesBridge;

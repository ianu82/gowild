use std::ffi::OsString;
use std::path::Path;

use crate::gateway::{AuthenticationMode, Credential, GatewayProtocol};

use super::{
    AdapterError, ChildEnvironment, CliAdapter, CodingCli, LaunchMode, LaunchRequest, LaunchSpec,
    ResolvedGateway,
};

const ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const ANTHROPIC_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_CUSTOM_HEADERS: &str = "ANTHROPIC_CUSTOM_HEADERS";
const ANTHROPIC_MODEL: &str = "ANTHROPIC_MODEL";
const ANTHROPIC_CUSTOM_MODEL_OPTION: &str = "ANTHROPIC_CUSTOM_MODEL_OPTION";
const ANTHROPIC_DEFAULT_HAIKU_MODEL: &str = "ANTHROPIC_DEFAULT_HAIKU_MODEL";
const CLAUDE_CODE_SUBAGENT_MODEL: &str = "CLAUDE_CODE_SUBAGENT_MODEL";
const ENABLE_GATEWAY_MODEL_DISCOVERY: &str = "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY";
const ENABLE_TOOL_SEARCH: &str = "ENABLE_TOOL_SEARCH";

const CLOUD_PROVIDER_FLAGS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

pub(crate) struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn cli(&self) -> CodingCli {
        CodingCli::Claude
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn executable_candidates(&self) -> &'static [&'static str] {
        &["claude"]
    }

    fn required_protocol(&self) -> GatewayProtocol {
        GatewayProtocol::AnthropicMessages
    }

    fn build(
        &self,
        executable: &Path,
        resolved: &ResolvedGateway,
        request: &LaunchRequest,
    ) -> Result<LaunchSpec, AdapterError> {
        validate_request(request)?;

        let mut environment = ChildEnvironment::default();
        set_plain(&mut environment, ANTHROPIC_BASE_URL, &resolved.endpoint)?;
        remove(&mut environment, ANTHROPIC_MODEL)?;
        for key in CLOUD_PROVIDER_FLAGS {
            remove(&mut environment, key)?;
        }

        configure_authentication(&mut environment, resolved)?;
        configure_custom_headers(&mut environment, resolved)?;
        configure_model_discovery(&mut environment, resolved)?;

        let mut args = request.passthrough_args.clone();
        if let Some(model) = &resolved.model {
            set_plain(&mut environment, ANTHROPIC_CUSTOM_MODEL_OPTION, model)?;
            // Claude uses a separate Haiku-class model for background work and
            // may choose models independently for subagents. Pin both to the
            // gateway model so a custom catalog never falls through to an
            // unavailable built-in Anthropic model id.
            set_plain(&mut environment, ANTHROPIC_DEFAULT_HAIKU_MODEL, model)?;
            set_plain(&mut environment, CLAUDE_CODE_SUBAGENT_MODEL, model)?;
            args.extend([OsString::from("--model"), OsString::from(model)]);
        } else {
            remove(&mut environment, ANTHROPIC_CUSTOM_MODEL_OPTION)?;
            remove(&mut environment, ANTHROPIC_DEFAULT_HAIKU_MODEL)?;
            remove(&mut environment, CLAUDE_CODE_SUBAGENT_MODEL)?;
        }
        if let LaunchMode::Resume { session_ref } = &request.mode {
            args.extend([OsString::from("--resume"), OsString::from(session_ref)]);
        }

        Ok(LaunchSpec::new(
            CodingCli::Claude,
            executable.into(),
            args,
            environment,
        ))
    }
}

fn validate_request(request: &LaunchRequest) -> Result<(), AdapterError> {
    if let LaunchMode::Resume { session_ref } = &request.mode {
        if session_ref.is_empty()
            || session_ref.len() > 512
            || session_ref.chars().any(char::is_control)
        {
            return Err(AdapterError::new("the Claude session reference is invalid"));
        }
    }

    if request.passthrough_args.iter().any(|argument| {
        let argument = argument.to_string_lossy();
        matches!(
            argument.as_ref(),
            "--" | "--bare"
                | "--bg"
                | "--background"
                | "--cloud"
                | "--continue"
                | "-c"
                | "--environment"
                | "--fork-session"
                | "--from-pr"
                | "--model"
                | "--resume"
                | "-r"
                | "--session-id"
                | "--teleport"
        ) || argument.starts_with("--model=")
            || argument.starts_with("--resume=")
            || argument.starts_with("--cloud=")
            || argument.starts_with("--environment=")
            || argument.starts_with("--from-pr=")
            || argument.starts_with("--session-id=")
            || argument.starts_with("--teleport=")
    }) {
        return Err(AdapterError::new(
            "Claude passthrough arguments cannot override the GoWild session, model, or gateway launch mode",
        ));
    }
    Ok(())
}

fn configure_authentication(
    environment: &mut ChildEnvironment,
    resolved: &ResolvedGateway,
) -> Result<(), AdapterError> {
    let credential = resolved.credential.as_ref();
    match resolved.gateway.auth.mode {
        AuthenticationMode::BearerToken => {
            set_secret(
                environment,
                ANTHROPIC_AUTH_TOKEN,
                required_credential(credential)?.clone(),
            )?;
            remove(environment, ANTHROPIC_API_KEY)?;
        }
        AuthenticationMode::XApiKey => {
            set_secret(
                environment,
                ANTHROPIC_API_KEY,
                required_credential(credential)?.clone(),
            )?;
            remove(environment, ANTHROPIC_AUTH_TOKEN)?;
        }
        AuthenticationMode::CustomHeader => {
            return Err(AdapterError::new(
                "Claude Code cannot safely use custom-header-only gateway authentication without also selecting a saved Anthropic credential",
            ));
        }
        AuthenticationMode::None => {
            return Err(AdapterError::new(
                "Claude Code cannot safely launch an unauthenticated gateway without risking fallback to saved Anthropic credentials",
            ));
        }
    }
    Ok(())
}

fn configure_custom_headers(
    environment: &mut ChildEnvironment,
    resolved: &ResolvedGateway,
) -> Result<(), AdapterError> {
    let headers = resolved
        .gateway
        .custom_headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>();

    if headers.is_empty() {
        remove(environment, ANTHROPIC_CUSTOM_HEADERS)?;
    } else {
        set_plain(environment, ANTHROPIC_CUSTOM_HEADERS, headers.join("\n"))?;
    }
    Ok(())
}

fn configure_model_discovery(
    environment: &mut ChildEnvironment,
    resolved: &ResolvedGateway,
) -> Result<(), AdapterError> {
    let expected_model_url = format!("{}/v1/models", resolved.endpoint.trim_end_matches('/'));
    let native_model_discovery = resolved.gateway.model_discovery.enabled
        && resolved
            .gateway
            .model_discovery
            .url
            .as_deref()
            .is_some_and(|url| url.trim_end_matches('/') == expected_model_url);
    if native_model_discovery {
        set_plain(environment, ENABLE_GATEWAY_MODEL_DISCOVERY, "1")?;
    } else {
        remove(environment, ENABLE_GATEWAY_MODEL_DISCOVERY)?;
    }

    if resolved
        .gateway
        .capabilities
        .features
        .contains(&crate::gateway::GatewayFeature::ToolCalling)
    {
        set_plain(environment, ENABLE_TOOL_SEARCH, "true")?;
    } else {
        remove(environment, ENABLE_TOOL_SEARCH)?;
    }
    Ok(())
}

fn required_credential(value: Option<&Credential>) -> Result<&Credential, AdapterError> {
    value.ok_or_else(|| AdapterError::new("the selected gateway credential is missing"))
}

fn set_plain(
    environment: &mut ChildEnvironment,
    key: &str,
    value: impl Into<String>,
) -> Result<(), AdapterError> {
    environment
        .set_plain(key, value)
        .map_err(|_| AdapterError::new("Claude produced an invalid launch environment"))
}

fn set_secret(
    environment: &mut ChildEnvironment,
    key: &str,
    value: Credential,
) -> Result<(), AdapterError> {
    environment
        .set_secret(key, value)
        .map_err(|_| AdapterError::new("Claude produced an invalid secret environment"))
}

fn remove(environment: &mut ChildEnvironment, key: &str) -> Result<(), AdapterError> {
    environment
        .remove(key)
        .map_err(|_| AdapterError::new("Claude produced a conflicting launch environment"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::gateway::{Gateway, GatewayFeature};

    use super::*;

    fn resolved_mindshub(auth_mode: AuthenticationMode) -> ResolvedGateway {
        let mut gateway = Gateway::mindshub();
        gateway.auth.mode = auth_mode;
        if auth_mode == AuthenticationMode::CustomHeader {
            gateway.auth.header_name = Some("X-Gateway-Token".into());
            gateway.auth.value_prefix = Some("Token ".into());
        }
        ResolvedGateway {
            gateway,
            protocol: GatewayProtocol::AnthropicMessages,
            endpoint: "https://api.mindshub.ai".into(),
            credential: (auth_mode != AuthenticationMode::None)
                .then(|| Credential::new("fake-mindshub-key").unwrap()),
            model: Some("sonnet".into()),
        }
    }

    fn explicit_environment(spec: &LaunchSpec) -> BTreeMap<String, Option<String>> {
        spec.command()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn mindshub_bearer_launch_uses_messages_contract_and_gateway_model() {
        let spec = ClaudeAdapter
            .build(
                Path::new("claude"),
                &resolved_mindshub(AuthenticationMode::BearerToken),
                &LaunchRequest::default(),
            )
            .unwrap();
        let environment = explicit_environment(&spec);

        assert_eq!(spec.args, ["--model", "sonnet"]);
        assert_eq!(
            environment.get(ANTHROPIC_BASE_URL),
            Some(&Some("https://api.mindshub.ai".into()))
        );
        assert_eq!(
            environment.get(ANTHROPIC_AUTH_TOKEN),
            Some(&Some("fake-mindshub-key".into()))
        );
        assert_eq!(environment.get(ANTHROPIC_API_KEY), Some(&None));
        assert_eq!(
            environment.get(ENABLE_GATEWAY_MODEL_DISCOVERY),
            Some(&Some("1".into()))
        );
        assert_eq!(
            environment.get(ANTHROPIC_CUSTOM_MODEL_OPTION),
            Some(&Some("sonnet".into()))
        );
        assert_eq!(
            environment.get(ANTHROPIC_DEFAULT_HAIKU_MODEL),
            Some(&Some("sonnet".into()))
        );
        assert_eq!(
            environment.get(CLAUDE_CODE_SUBAGENT_MODEL),
            Some(&Some("sonnet".into()))
        );
        assert_eq!(
            environment.get(ENABLE_TOOL_SEARCH),
            Some(&Some("true".into()))
        );
        assert!(!format!("{spec:?}").contains("fake-mindshub-key"));
    }

    #[test]
    fn x_api_key_and_non_secret_custom_headers_are_translated() {
        let api_key_spec = ClaudeAdapter
            .build(
                Path::new("claude"),
                &resolved_mindshub(AuthenticationMode::XApiKey),
                &LaunchRequest::default(),
            )
            .unwrap();
        let api_key_environment = explicit_environment(&api_key_spec);
        assert_eq!(
            api_key_environment.get(ANTHROPIC_API_KEY),
            Some(&Some("fake-mindshub-key".into()))
        );
        assert_eq!(api_key_environment.get(ANTHROPIC_AUTH_TOKEN), Some(&None));

        let mut custom_headers = resolved_mindshub(AuthenticationMode::BearerToken);
        custom_headers
            .gateway
            .custom_headers
            .insert("X-Workspace".into(), "engineering".into());
        let custom_spec = ClaudeAdapter
            .build(
                Path::new("claude"),
                &custom_headers,
                &LaunchRequest::default(),
            )
            .unwrap();
        assert_eq!(
            explicit_environment(&custom_spec).get(ANTHROPIC_CUSTOM_HEADERS),
            Some(&Some("X-Workspace: engineering".into()))
        );
    }

    #[test]
    fn resume_keeps_gateway_environment_and_adds_only_resume_arguments() {
        let resolved = resolved_mindshub(AuthenticationMode::BearerToken);
        let fresh = ClaudeAdapter
            .build(Path::new("claude"), &resolved, &LaunchRequest::default())
            .unwrap();
        let resumed = ClaudeAdapter
            .build(
                Path::new("claude"),
                &resolved,
                &LaunchRequest {
                    mode: LaunchMode::Resume {
                        session_ref: "session-42".into(),
                    },
                    ..LaunchRequest::default()
                },
            )
            .unwrap();

        assert_eq!(explicit_environment(&fresh), explicit_environment(&resumed));
        assert_eq!(
            resumed.args,
            ["--model", "sonnet", "--resume", "session-42"]
        );
    }

    #[test]
    fn unsafe_auth_modes_and_launch_overrides_fail_closed() {
        for auth_mode in [AuthenticationMode::None, AuthenticationMode::CustomHeader] {
            assert!(ClaudeAdapter
                .build(
                    Path::new("claude"),
                    &resolved_mindshub(auth_mode),
                    &LaunchRequest::default(),
                )
                .unwrap_err()
                .to_string()
                .contains("cannot safely"));
        }

        let request = LaunchRequest {
            passthrough_args: vec![OsString::from("--model=override")],
            ..LaunchRequest::default()
        };
        assert!(ClaudeAdapter
            .build(
                Path::new("claude"),
                &resolved_mindshub(AuthenticationMode::BearerToken),
                &request,
            )
            .is_err());
    }

    #[test]
    fn inherited_cloud_provider_modes_are_removed() {
        let mut resolved = resolved_mindshub(AuthenticationMode::BearerToken);
        resolved.gateway.model_discovery.enabled = false;
        resolved
            .gateway
            .capabilities
            .features
            .remove(&GatewayFeature::ToolCalling);
        let spec = ClaudeAdapter
            .build(Path::new("claude"), &resolved, &LaunchRequest::default())
            .unwrap();
        let environment = explicit_environment(&spec);
        for key in CLOUD_PROVIDER_FLAGS {
            assert_eq!(environment.get(*key), Some(&None));
        }
        assert_eq!(environment.get(ENABLE_GATEWAY_MODEL_DISCOVERY), Some(&None));
        assert_eq!(environment.get(ENABLE_TOOL_SEARCH), Some(&None));
    }
}

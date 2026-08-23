use std::ffi::OsString;
use std::path::Path;

use crate::gateway::{AuthenticationMode, Credential, GatewayProtocol};

use super::{
    AdapterError, ChildEnvironment, CliAdapter, CodingCli, LaunchMode, LaunchRequest, LaunchSpec,
    ResolvedGateway,
};

const PROVIDER_ID: &str = "gowild";
const GOWILD_CODEX_API_KEY: &str = "GOWILD_CODEX_API_KEY";
const GOWILD_API_KEY: &str = "GOWILD_API_KEY";

// These credentials select OpenAI-owned authentication paths in current
// Codex releases. GoWild's custom provider must never inherit them.
const INHERITED_CODEX_CREDENTIALS: &[&str] = &[
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_API_KEY",
    GOWILD_API_KEY,
];

pub(crate) struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn cli(&self) -> CodingCli {
        CodingCli::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }

    fn executable_candidates(&self) -> &'static [&'static str] {
        &["codex"]
    }

    fn required_protocol(&self) -> GatewayProtocol {
        GatewayProtocol::OpenAiResponses
    }

    fn build(
        &self,
        executable: &Path,
        resolved: &ResolvedGateway,
        request: &LaunchRequest,
    ) -> Result<LaunchSpec, AdapterError> {
        validate_request(request)?;

        let mut environment = ChildEnvironment::default();
        for key in INHERITED_CODEX_CREDENTIALS {
            remove(&mut environment, key)?;
        }

        let mut provider_overrides = ProviderOverrides::new(resolved);
        configure_authentication(&mut environment, &mut provider_overrides, resolved)?;
        provider_overrides.add_static_headers(&resolved.gateway.custom_headers);

        let mut args = Vec::new();
        // Clear any user-defined provider with GoWild's reserved id before
        // applying the exact launch contract. This prevents stale provider
        // fields (including headers) from surviving across gateway changes.
        push_config(&mut args, "model_providers.gowild", "{}");
        provider_overrides.append_to(&mut args);
        // Codex snapshots its shell environment by default. The gateway key
        // must remain available to Codex's HTTP client but must never reach a
        // shell tool or a persisted shell snapshot.
        push_config(&mut args, "features.shell_snapshot", "false");
        push_config(
            &mut args,
            "shell_environment_policy.ignore_default_excludes",
            "false",
        );
        push_string_config(&mut args, "model_provider", PROVIDER_ID);
        if let Some(model) = &resolved.model {
            push_string_config(&mut args, "model", model);
        }

        args.extend(request.passthrough_args.iter().cloned());
        if let LaunchMode::Resume { session_ref } = &request.mode {
            args.extend([OsString::from("resume"), OsString::from(session_ref)]);
        }

        Ok(LaunchSpec::new(
            CodingCli::Codex,
            executable.into(),
            args,
            environment,
        ))
    }
}

struct ProviderOverrides {
    name: String,
    base_url: String,
    env_key: Option<&'static str>,
    env_http_headers: Vec<(String, String)>,
    http_headers: Vec<(String, String)>,
}

impl ProviderOverrides {
    fn new(resolved: &ResolvedGateway) -> Self {
        Self {
            name: resolved.gateway.display_name.clone(),
            base_url: resolved.endpoint.clone(),
            env_key: None,
            env_http_headers: Vec::new(),
            http_headers: Vec::new(),
        }
    }

    fn add_static_headers(&mut self, headers: &std::collections::BTreeMap<String, String>) {
        self.http_headers.extend(
            headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
    }

    fn append_to(self, args: &mut Vec<OsString>) {
        push_string_config(args, "model_providers.gowild.name", &self.name);
        push_string_config(args, "model_providers.gowild.base_url", &self.base_url);
        push_string_config(args, "model_providers.gowild.wire_api", "responses");
        push_config(args, "model_providers.gowild.requires_openai_auth", "false");

        if let Some(env_key) = self.env_key {
            push_string_config(args, "model_providers.gowild.env_key", env_key);
        }
        if !self.env_http_headers.is_empty() {
            push_config(
                args,
                "model_providers.gowild.env_http_headers",
                &toml_string_map(&self.env_http_headers),
            );
        }
        if !self.http_headers.is_empty() {
            push_config(
                args,
                "model_providers.gowild.http_headers",
                &toml_string_map(&self.http_headers),
            );
        }
    }
}

fn configure_authentication(
    environment: &mut ChildEnvironment,
    provider: &mut ProviderOverrides,
    resolved: &ResolvedGateway,
) -> Result<(), AdapterError> {
    match resolved.gateway.auth.mode {
        AuthenticationMode::BearerToken => {
            set_secret(
                environment,
                GOWILD_CODEX_API_KEY,
                required_credential(resolved)?.clone(),
            )?;
            provider.env_key = Some(GOWILD_CODEX_API_KEY);
        }
        AuthenticationMode::XApiKey => {
            set_secret(
                environment,
                GOWILD_CODEX_API_KEY,
                required_credential(resolved)?.clone(),
            )?;
            provider
                .env_http_headers
                .push(("x-api-key".into(), GOWILD_CODEX_API_KEY.into()));
        }
        AuthenticationMode::CustomHeader => {
            let header_name =
                resolved.gateway.auth.header_name.clone().ok_or_else(|| {
                    AdapterError::new("the custom authentication header is missing")
                })?;
            if resolved
                .gateway
                .custom_headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case(&header_name))
            {
                return Err(AdapterError::new(
                    "the custom authentication header conflicts with a non-secret gateway header",
                ));
            }
            let credential = required_credential(resolved)?;
            let secret = format!(
                "{}{}",
                resolved.gateway.auth.value_prefix.as_deref().unwrap_or(""),
                credential.expose()
            );
            let secret = Credential::new(secret).map_err(|_| {
                AdapterError::new("the custom authentication header value is invalid")
            })?;
            set_secret(environment, GOWILD_CODEX_API_KEY, secret)?;
            provider
                .env_http_headers
                .push((header_name, GOWILD_CODEX_API_KEY.into()));
        }
        AuthenticationMode::None => {
            remove(environment, GOWILD_CODEX_API_KEY)?;
        }
    }
    Ok(())
}

fn validate_request(request: &LaunchRequest) -> Result<(), AdapterError> {
    if let LaunchMode::Resume { session_ref } = &request.mode {
        if session_ref.is_empty()
            || session_ref.starts_with('-')
            || session_ref.len() > 512
            || session_ref.chars().any(char::is_control)
        {
            return Err(AdapterError::new("the Codex session reference is invalid"));
        }
    }

    if request.passthrough_args.iter().any(|argument| {
        let argument = argument.to_string_lossy();
        matches!(
            argument.as_ref(),
            "--" | "-c"
                | "--config"
                | "-m"
                | "--model"
                | "--oss"
                | "--local-provider"
                | "--remote"
                | "--remote-auth-token-env"
                | "app"
                | "app-server"
                | "cloud"
                | "exec"
                | "exec-server"
                | "fork"
                | "remote-control"
                | "resume"
                | "review"
        ) || (argument.starts_with("-c") && argument != "-C")
            || argument.starts_with("-m")
            || argument.starts_with("--config=")
            || argument.starts_with("--model=")
            || argument.starts_with("--local-provider=")
            || argument.starts_with("--remote=")
            || argument.starts_with("--remote-auth-token-env=")
    }) {
        return Err(AdapterError::new(
            "Codex passthrough arguments cannot override the GoWild model, provider, or local launch mode",
        ));
    }
    Ok(())
}

fn required_credential(resolved: &ResolvedGateway) -> Result<&Credential, AdapterError> {
    resolved
        .credential
        .as_ref()
        .ok_or_else(|| AdapterError::new("the selected gateway credential is missing"))
}

fn push_config(args: &mut Vec<OsString>, key: &str, value: &str) {
    args.extend([
        OsString::from("-c"),
        OsString::from(format!("{key}={value}")),
    ]);
}

fn push_string_config(args: &mut Vec<OsString>, key: &str, value: &str) {
    push_config(args, key, &toml_string(value));
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.into()).to_string()
}

fn toml_string_map(entries: &[(String, String)]) -> String {
    let entries = entries
        .iter()
        .map(|(name, value)| format!("{}={}", toml_string(name), toml_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{entries}}}")
}

fn set_secret(
    environment: &mut ChildEnvironment,
    key: &str,
    value: Credential,
) -> Result<(), AdapterError> {
    environment
        .set_secret(key, value)
        .map_err(|_| AdapterError::new("Codex produced an invalid secret environment"))
}

fn remove(environment: &mut ChildEnvironment, key: &str) -> Result<(), AdapterError> {
    environment
        .remove(key)
        .map_err(|_| AdapterError::new("Codex produced a conflicting launch environment"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::gateway::Gateway;

    use super::*;

    fn resolved_mindshub(auth_mode: AuthenticationMode) -> ResolvedGateway {
        let mut gateway = Gateway::mindshub();
        gateway.auth.mode = auth_mode;
        match auth_mode {
            AuthenticationMode::CustomHeader => {
                gateway.auth.header_name = Some("X-Gateway-Token".into());
                gateway.auth.value_prefix = Some("Token ".into());
            }
            AuthenticationMode::None => {
                gateway.auth.credential_ref = None;
            }
            _ => {}
        }
        ResolvedGateway {
            gateway,
            protocol: GatewayProtocol::OpenAiResponses,
            endpoint: "https://api.mindshub.ai/v1".into(),
            credential: (auth_mode != AuthenticationMode::None)
                .then(|| Credential::new("fake-mindshub-key").unwrap()),
            model: Some("gpt-coding".into()),
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

    fn string_args(spec: &LaunchSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn mindshub_bearer_launch_uses_responses_provider_and_gateway_model() {
        let spec = CodexAdapter
            .build(
                Path::new("codex"),
                &resolved_mindshub(AuthenticationMode::BearerToken),
                &LaunchRequest::default(),
            )
            .unwrap();
        let args = string_args(&spec);
        let environment = explicit_environment(&spec);

        for expected in [
            "model_providers.gowild={}",
            "model_providers.gowild.name=\"MindsHub Inference\"",
            "model_providers.gowild.base_url=\"https://api.mindshub.ai/v1\"",
            "model_providers.gowild.wire_api=\"responses\"",
            "model_providers.gowild.requires_openai_auth=false",
            "model_providers.gowild.env_key=\"GOWILD_CODEX_API_KEY\"",
            "features.shell_snapshot=false",
            "shell_environment_policy.ignore_default_excludes=false",
            "model_provider=\"gowild\"",
            "model=\"gpt-coding\"",
        ] {
            assert!(args.contains(&expected.into()), "missing {expected:?}");
        }
        assert_eq!(
            environment.get(GOWILD_CODEX_API_KEY),
            Some(&Some("fake-mindshub-key".into()))
        );
        for key in INHERITED_CODEX_CREDENTIALS {
            assert_eq!(environment.get(*key), Some(&None));
        }
        assert!(!args.iter().any(|arg| arg.contains("fake-mindshub-key")));
        assert!(!format!("{spec:?}").contains("fake-mindshub-key"));
    }

    #[test]
    fn header_authentication_and_static_headers_stay_out_of_argv() {
        let mut x_api_key = resolved_mindshub(AuthenticationMode::XApiKey);
        x_api_key
            .gateway
            .custom_headers
            .insert("X-Workspace".into(), "engineering".into());
        let x_api_key_spec = CodexAdapter
            .build(Path::new("codex"), &x_api_key, &LaunchRequest::default())
            .unwrap();
        let x_api_key_args = string_args(&x_api_key_spec);
        assert!(x_api_key_args.contains(
            &"model_providers.gowild.env_http_headers={\"x-api-key\"=\"GOWILD_CODEX_API_KEY\"}"
                .into()
        ));
        assert!(x_api_key_args.contains(
            &"model_providers.gowild.http_headers={\"X-Workspace\"=\"engineering\"}".into()
        ));

        let custom = CodexAdapter
            .build(
                Path::new("codex"),
                &resolved_mindshub(AuthenticationMode::CustomHeader),
                &LaunchRequest::default(),
            )
            .unwrap();
        assert!(string_args(&custom).contains(
            &"model_providers.gowild.env_http_headers={\"X-Gateway-Token\"=\"GOWILD_CODEX_API_KEY\"}"
                .into()
        ));
        assert_eq!(
            explicit_environment(&custom).get(GOWILD_CODEX_API_KEY),
            Some(&Some("Token fake-mindshub-key".into()))
        );
        assert!(!string_args(&custom)
            .iter()
            .any(|arg| arg.contains("fake-mindshub-key")));
    }

    #[test]
    fn unauthenticated_provider_has_no_auth_fields_or_secret() {
        let spec = CodexAdapter
            .build(
                Path::new("codex"),
                &resolved_mindshub(AuthenticationMode::None),
                &LaunchRequest::default(),
            )
            .unwrap();
        let args = string_args(&spec);
        assert!(!args.iter().any(|arg| {
            arg.contains("env_key")
                || arg.contains("env_http_headers")
                || arg.contains("experimental_bearer_token")
        }));
        assert_eq!(
            explicit_environment(&spec).get(GOWILD_CODEX_API_KEY),
            Some(&None)
        );
    }

    #[test]
    fn resume_keeps_provider_contract_and_uses_native_subcommand() {
        let resolved = resolved_mindshub(AuthenticationMode::BearerToken);
        let fresh = CodexAdapter
            .build(Path::new("codex"), &resolved, &LaunchRequest::default())
            .unwrap();
        let resumed = CodexAdapter
            .build(
                Path::new("codex"),
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
        let mut expected = string_args(&fresh);
        expected.extend(["resume".into(), "session-42".into()]);
        assert_eq!(string_args(&resumed), expected);
    }

    #[test]
    fn launch_overrides_and_invalid_resume_references_fail_closed() {
        for argument in [
            "--model=override",
            "-mother-model",
            "--config",
            "-cmodel_provider=\"openai\"",
            "--oss",
            "remote-control",
        ] {
            let request = LaunchRequest {
                passthrough_args: vec![OsString::from(argument)],
                ..LaunchRequest::default()
            };
            assert!(CodexAdapter
                .build(
                    Path::new("codex"),
                    &resolved_mindshub(AuthenticationMode::BearerToken),
                    &request,
                )
                .is_err());
        }

        let request = LaunchRequest {
            mode: LaunchMode::Resume {
                session_ref: "bad\nsession".into(),
            },
            ..LaunchRequest::default()
        };
        assert!(CodexAdapter
            .build(
                Path::new("codex"),
                &resolved_mindshub(AuthenticationMode::BearerToken),
                &request,
            )
            .is_err());

        let option_like_session = LaunchRequest {
            mode: LaunchMode::Resume {
                session_ref: "--last".into(),
            },
            ..LaunchRequest::default()
        };
        assert!(CodexAdapter
            .build(
                Path::new("codex"),
                &resolved_mindshub(AuthenticationMode::BearerToken),
                &option_like_session,
            )
            .is_err());

        let mut conflicting_header = resolved_mindshub(AuthenticationMode::CustomHeader);
        conflicting_header
            .gateway
            .custom_headers
            .insert("x-gateway-token".into(), "not-secret".into());
        assert!(CodexAdapter
            .build(
                Path::new("codex"),
                &conflicting_header,
                &LaunchRequest::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("conflicts"));
    }

    #[test]
    fn config_strings_and_header_maps_are_toml_quoted() {
        let serialized = toml_string("a \"quoted\" value");
        let parsed = format!("value={serialized}")
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(parsed["value"].as_str(), Some("a \"quoted\" value"));
        let serialized =
            toml_string_map(&[("X.Custom-Header".into(), "a \"quoted\" value".into())]);
        let parsed = format!("headers={serialized}")
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(
            parsed["headers"]["X.Custom-Header"].as_str(),
            Some("a \"quoted\" value")
        );
    }
}

use std::ffi::OsString;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Mode};
use crate::cli_adapter::{
    AdapterRegistry, Environment, ExecutableLocator, LaunchMode, LaunchPlanner, LaunchRequest,
    PathExecutableLocator,
};
use crate::pane::PaneLaunchEnv;

mod state;
pub(crate) use state::{CodingAgentLaunchField, CodingAgentLaunchState};

struct ConfiguredEnvironment;

impl Environment for ConfiguredEnvironment {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

impl App {
    pub(crate) fn handle_coding_agent_launch_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.close_coding_agent_launch(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.coding_agent_launch.move_field(-1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.state.coding_agent_launch.move_field(1);
            }
            KeyCode::BackTab => self.state.coding_agent_launch.move_field(-1),
            KeyCode::Left | KeyCode::Char('h') => self
                .state
                .coding_agent_launch
                .cycle_selected(&self.state.gateway_catalog, -1),
            KeyCode::Right | KeyCode::Char('l') => self
                .state
                .coding_agent_launch
                .cycle_selected(&self.state.gateway_catalog, 1),
            KeyCode::Char('s') => crate::app::input::open_settings_at(
                &mut self.state,
                crate::app::state::SettingsSection::Gateways,
            ),
            KeyCode::Enter => self.launch_selected_coding_agent(),
            _ => {}
        }
    }

    fn close_coding_agent_launch(&mut self) {
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
        self.state.coding_agent_launch.error = None;
    }

    pub(crate) fn launch_selected_coding_agent(&mut self) {
        let locator = PathExecutableLocator;
        if let Err(error) = self.launch_selected_coding_agent_with(&locator) {
            self.state.coding_agent_launch.error = Some(error);
        }
    }

    fn launch_selected_coding_agent_with(
        &mut self,
        locator: &dyn ExecutableLocator,
    ) -> Result<(), String> {
        let selection = self.state.coding_agent_launch.clone();
        let gateway = selection
            .gateway(&self.state.gateway_catalog)
            .ok_or_else(|| {
                format!(
                    "No configured gateway supports {}.",
                    selection.protocol().display_name()
                )
            })?;
        let model = selection.model.clone().ok_or_else(|| {
            "No model is selected. Test the gateway in Settings, then choose a model.".to_string()
        })?;
        let gateway_id = gateway.id.clone();
        let gateway_name = gateway.display_name.clone();
        let protocol = selection.protocol();

        let registry = AdapterRegistry::with_builtin_adapters();
        let environment = ConfiguredEnvironment;
        let resolver = crate::cli_adapter::GatewayResolver::new(
            &self.state.gateway_catalog,
            self.gateway_credentials.as_ref(),
            &environment,
        );
        let planner = LaunchPlanner::new(&registry, resolver, locator);
        let request = LaunchRequest {
            gateway_id: Some(gateway_id.clone()),
            model: Some(model.clone()),
            mode: LaunchMode::Fresh,
            passthrough_args: Vec::new(),
        };
        let spec = planner
            .plan(selection.cli, &request)
            .map_err(|error| error.to_string())?;
        let (argv, launch_env) = pane_command_parts(spec.into_pane_parts())?;

        self.spawn_coding_agent_tab(
            &argv,
            launch_env,
            &selection,
            &gateway_id,
            &gateway_name,
            protocol,
            &model,
        )?;
        self.state.coding_agent_launch.error = None;
        Ok(())
    }

    fn spawn_coding_agent_tab(
        &mut self,
        argv: &[String],
        launch_env: PaneLaunchEnv,
        selection: &CodingAgentLaunchState,
        gateway_id: &str,
        gateway_name: &str,
        protocol: crate::gateway::GatewayProtocol,
        model: &str,
    ) -> Result<(), String> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| "Open or create a workspace before launching an agent.".to_string())?;
        let cwd = self
            .focused_pane_cwd_in_workspace(ws_idx)
            .or_else(|| self.seed_cwd_from_workspace(ws_idx))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        let (rows, cols) = self.state.estimate_pane_size();
        let (tab_idx, terminal, runtime) = self.state.workspaces[ws_idx]
            .create_tab_argv_command_with_launch_env(
                rows,
                cols,
                cwd,
                argv,
                launch_env,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                self.state.host_terminal_appearance,
            )
            .map_err(|error| format!("Could not launch {}: {error}", selection.cli_label()))?;
        let terminal = terminal.with_gateway_agent_route(crate::terminal::GatewayAgentRoute {
            cli: selection.cli.id().to_string(),
            gateway_id: gateway_id.to_string(),
            model: model.to_string(),
        });

        let root_pane = self.state.workspaces[ws_idx].tabs[tab_idx].root_pane;
        self.state.workspaces[ws_idx].tabs[tab_idx]
            .set_custom_name(selection.cli_label().to_string());
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.remove_alias_shadowed_by_new_pane(root_pane);
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.mode = Mode::Terminal;
        self.emit_tab_created_events(ws_idx, tab_idx);
        self.schedule_session_save();

        tracing::info!(
            cli = selection.cli.id(),
            gateway = gateway_name,
            protocol = protocol.display_name(),
            model,
            "launched coding agent through configured gateway"
        );
        Ok(())
    }
}

fn pane_command_parts(
    (executable, args, launch_env): (PathBuf, Vec<OsString>, PaneLaunchEnv),
) -> Result<(Vec<String>, PaneLaunchEnv), String> {
    let executable = executable
        .into_os_string()
        .into_string()
        .map_err(|_| "The coding CLI executable path is not valid Unicode.".to_string())?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(executable);
    for argument in args {
        argv.push(
            argument
                .into_string()
                .map_err(|_| "A coding CLI launch argument is not valid Unicode.".to_string())?,
        );
    }
    Ok((argv, launch_env))
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::cli_adapter::CodingCli;
    use crate::gateway::{
        CachedModel, Credential, CredentialBackend, CredentialStore, CredentialStoreError,
        GatewayCatalog,
    };

    struct FixedLocator(PathBuf);

    impl ExecutableLocator for FixedLocator {
        fn locate(&self, _candidates: &[&str]) -> Option<PathBuf> {
            Some(self.0.clone())
        }
    }

    struct MemoryCredentialStore(Credential);

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
            Ok((credential_ref == "gateway:mindshub").then(|| self.0.clone()))
        }

        fn set(
            &self,
            _credential_ref: &str,
            _credential: &Credential,
        ) -> Result<CredentialBackend, CredentialStoreError> {
            Ok(CredentialBackend::RestrictedFile)
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), CredentialStoreError> {
            Ok(())
        }
    }

    struct MissingCredentialStore;

    impl CredentialStore for MissingCredentialStore {
        fn get(&self, _credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
            Ok(None)
        }

        fn set(
            &self,
            _credential_ref: &str,
            _credential: &Credential,
        ) -> Result<CredentialBackend, CredentialStoreError> {
            unreachable!()
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), CredentialStoreError> {
            unreachable!()
        }
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gowild-{label}-{}-{stamp}", std::process::id()))
    }

    fn wait_for_file(path: &std::path::Path) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok(value) = fs::read_to_string(path) {
                return value;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {}", path.display());
    }

    fn active_gateway_route(app: &App) -> Option<&crate::terminal::GatewayAgentRoute> {
        let workspace = app.state.workspaces.get(app.state.active?)?;
        let tab = workspace.active_tab()?;
        let pane = tab.panes.get(&tab.root_pane)?;
        app.state
            .terminals
            .get(&pane.attached_terminal_id)?
            .gateway_agent_route
            .as_ref()
    }

    #[test]
    fn launch_selection_is_explicit_and_cli_specific() {
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("mindshub".into());
        let gateway = catalog.gateways.get_mut("mindshub").unwrap();
        gateway.model_discovery.cached_models = vec![CachedModel {
            id: "shared-model".into(),
            label: None,
            provider: None,
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];
        gateway
            .default_models
            .insert("codex".into(), "codex-model".into());
        gateway
            .default_models
            .insert("claude".into(), "claude-model".into());

        let mut selection = CodingAgentLaunchState::new(&catalog);
        assert_eq!(selection.cli, CodingCli::Codex);
        assert_eq!(selection.gateway_id.as_deref(), Some("mindshub"));
        assert_eq!(selection.model.as_deref(), Some("codex-model"));
        assert_eq!(
            selection.protocol(),
            crate::gateway::GatewayProtocol::OpenAiResponses
        );

        selection.cycle_selected(&catalog, 1);
        assert_eq!(selection.cli, CodingCli::Claude);
        assert_eq!(selection.model.as_deref(), Some("claude-model"));
        assert_eq!(
            selection.protocol(),
            crate::gateway::GatewayProtocol::AnthropicMessages
        );
    }

    #[test]
    fn selection_has_no_implicit_model_when_gateway_has_none() {
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("mindshub".into());

        let selection = CodingAgentLaunchState::new(&catalog);

        assert_eq!(selection.gateway_id.as_deref(), Some("mindshub"));
        assert_eq!(selection.model, None);
    }

    #[test]
    fn keyboard_selector_cycles_cli_and_opens_gateway_settings() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let gateway = app
            .state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap();
        gateway
            .default_models
            .insert("codex".into(), "codex-model".into());
        gateway
            .default_models
            .insert("claude".into(), "claude-model".into());
        app.state.coding_agent_launch = CodingAgentLaunchState::new(&app.state.gateway_catalog);
        app.state.mode = Mode::CodingAgentLaunch;
        app.state.settings.section = crate::app::state::SettingsSection::Theme;

        app.handle_coding_agent_launch_key(KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.state.coding_agent_launch.cli, CodingCli::Claude);
        assert_eq!(
            app.state.coding_agent_launch.model.as_deref(),
            Some("claude-model")
        );

        app.handle_coding_agent_launch_key(KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.state.mode, Mode::Settings);
        assert_eq!(
            app.state.settings.section,
            crate::app::state::SettingsSection::Gateways
        );
    }

    #[test]
    fn missing_credential_fails_before_creating_a_tab() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.gateway_credentials = Box::new(MissingCredentialStore);
        app.state.gateway_catalog.default_gateway_id = Some("mindshub".into());
        app.state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .default_models
            .insert("codex".into(), "route-model".into());
        app.state.coding_agent_launch = CodingAgentLaunchState::new(&app.state.gateway_catalog);
        let before = app.state.workspaces.len();

        let error = app
            .launch_selected_coding_agent_with(&FixedLocator(PathBuf::from("/bin/false")))
            .unwrap_err();

        assert!(error.contains("no credential configured"));
        assert_eq!(app.state.workspaces.len(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_children_receive_selected_routes_without_proprietary_fallbacks() {
        let output_path = unique_temp_path("agent-route-output");
        let executable_path = unique_temp_path("agent-route-stub");
        let script = format!(
            "#!/bin/sh\n{{\n  printf 'args=%s\\n' \"$*\"\n  printf 'codex_secret=%s\\n' \"${{GOWILD_CODEX_API_KEY:+present}}\"\n  printf 'openai_secret=%s\\n' \"${{OPENAI_API_KEY:+present}}\"\n  printf 'anthropic_base=%s\\n' \"${{ANTHROPIC_BASE_URL:-absent}}\"\n  printf 'anthropic_token=%s\\n' \"${{ANTHROPIC_AUTH_TOKEN:+present}}\"\n  printf 'bedrock=%s\\n' \"${{CLAUDE_CODE_USE_BEDROCK:+present}}\"\n}} > '{}'\n",
            output_path.display()
        );
        fs::write(&executable_path, script).unwrap();
        let mut permissions = fs::metadata(&executable_path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable_path, permissions).unwrap();

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.mode = Mode::Navigate;
        assert!(app.ensure_default_workspace());
        app.gateway_credentials = Box::new(MemoryCredentialStore(
            Credential::new("test-route-secret").unwrap(),
        ));
        app.state.gateway_catalog.default_gateway_id = Some("mindshub".into());
        let gateway = app
            .state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap();
        gateway.endpoints.openai_responses = Some("https://route.invalid/openai/v1".into());
        gateway.endpoints.anthropic_messages = Some("https://route.invalid/anthropic".into());
        gateway
            .default_models
            .insert("codex".into(), "codex-route-model".into());
        gateway
            .default_models
            .insert("claude".into(), "claude-route-model".into());
        app.state.coding_agent_launch = CodingAgentLaunchState::new(&app.state.gateway_catalog);

        let locator = FixedLocator(executable_path.clone());
        app.launch_selected_coding_agent_with(&locator).unwrap();
        let codex = wait_for_file(&output_path);
        assert!(codex.contains("model_provider=\"gowild\""));
        assert!(codex.contains("https://route.invalid/openai/v1"));
        assert!(codex.contains("codex-route-model"));
        assert!(codex.contains("codex_secret=present"));
        assert!(codex.contains("openai_secret="));
        assert!(!codex.contains("test-route-secret"));
        assert_eq!(
            active_gateway_route(&app),
            Some(&crate::terminal::GatewayAgentRoute {
                cli: "codex".into(),
                gateway_id: "mindshub".into(),
                model: "codex-route-model".into(),
            })
        );

        fs::remove_file(&output_path).unwrap();
        app.state
            .coding_agent_launch
            .cycle_selected(&app.state.gateway_catalog, 1);
        app.launch_selected_coding_agent_with(&locator).unwrap();
        let claude = wait_for_file(&output_path);
        assert!(claude.contains("--model claude-route-model"));
        assert!(claude.contains("anthropic_base=https://route.invalid/anthropic"));
        assert!(claude.contains("anthropic_token=present"));
        assert!(claude.contains("bedrock="));
        assert!(!claude.contains("test-route-secret"));
        assert_eq!(
            active_gateway_route(&app),
            Some(&crate::terminal::GatewayAgentRoute {
                cli: "claude".into(),
                gateway_id: "mindshub".into(),
                model: "claude-route-model".into(),
            })
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = fs::remove_file(output_path);
        let _ = fs::remove_file(executable_path);
    }
}

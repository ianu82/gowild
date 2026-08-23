use super::App;

impl App {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_pending_gateway_agent_resume(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        plan: crate::agent_resume::AgentResumePlan,
        route: crate::terminal::GatewayAgentRoute,
        persisted_session: Option<crate::agent_resume::PersistedAgentSession>,
        rows: u16,
        cols: u16,
        locator: &dyn crate::cli_adapter::ExecutableLocator,
    ) -> bool {
        let launch =
            self.plan_gateway_agent_resume(&plan, &route, persisted_session.as_ref(), locator);
        let (argv, launch_env) = match launch {
            Ok(launch) => launch,
            Err(error) => {
                return self.start_gateway_resume_failure_shell(
                    pane_id,
                    terminal_id,
                    cwd,
                    &route,
                    rows,
                    cols,
                    &error,
                );
            }
        };
        let Some((ws_idx, _)) = self.find_pane(pane_id) else {
            return false;
        };
        let Some(launch_env) = self.identify_pane_launch_env(ws_idx, pane_id, launch_env) else {
            return false;
        };

        let runtime = match crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            rows,
            cols,
            cwd.clone(),
            &argv,
            &launch_env,
            crate::pane::AgentDetection::Enabled,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                return self.start_gateway_resume_failure_shell(
                    pane_id,
                    terminal_id,
                    cwd,
                    &route,
                    rows,
                    cols,
                    &format!("could not start {}: {error}", route.cli),
                );
            }
        };

        tracing::info!(
            pane = pane_id.raw(),
            terminal = %terminal_id,
            cli = route.cli,
            gateway = route.gateway_id,
            model = route.model,
            "resumed coding agent through persisted gateway route"
        );
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_agent_resume_plan = None;
            terminal.respawn_shell_on_exit = false;
        }
        true
    }

    fn plan_gateway_agent_resume(
        &mut self,
        plan: &crate::agent_resume::AgentResumePlan,
        route: &crate::terminal::GatewayAgentRoute,
        persisted_session: Option<&crate::agent_resume::PersistedAgentSession>,
        locator: &dyn crate::cli_adapter::ExecutableLocator,
    ) -> Result<(Vec<String>, crate::pane::PaneLaunchEnv), String> {
        let cli = crate::cli_adapter::CodingCli::from_id(&route.cli)
            .ok_or_else(|| format!("unsupported managed CLI `{}`", route.cli))?;
        if plan.agent != route.cli {
            return Err("the saved agent and gateway route do not match".to_string());
        }
        let persisted_session = persisted_session
            .ok_or_else(|| "the saved agent session is unavailable".to_string())?;
        if persisted_session.agent != route.cli
            || persisted_session.session_ref.kind != crate::agent_resume::AgentSessionRefKind::Id
        {
            return Err("the saved session is incompatible with the gateway route".to_string());
        }

        self.plan_coding_agent_launch(
            cli,
            &route.gateway_id,
            &route.model,
            crate::cli_adapter::LaunchMode::Resume {
                session_ref: persisted_session.session_ref.value.clone(),
            },
            locator,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_gateway_resume_failure_shell(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        route: &crate::terminal::GatewayAgentRoute,
        rows: u16,
        cols: u16,
        error: &str,
    ) -> bool {
        let Some((ws_idx, _)) = self.find_pane(pane_id) else {
            return false;
        };
        let Some(launch_env) = self.pane_launch_env(ws_idx, pane_id, Vec::new()) else {
            return false;
        };
        let history = gateway_resume_failure_history(route, error);
        let runtime = match crate::terminal::TerminalRuntime::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            Some(&history),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(spawn_error) => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    cli = route.cli,
                    gateway = route.gateway_id,
                    model = route.model,
                    err = %spawn_error,
                    "failed to start safe shell after managed agent resume failure"
                );
                return false;
            }
        };

        tracing::warn!(
            pane = pane_id.raw(),
            terminal = %terminal_id,
            cli = route.cli,
            gateway = route.gateway_id,
            model = route.model,
            error = %sanitized_resume_text(error),
            "managed agent resume failed closed"
        );
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_agent_resume_plan = None;
            terminal.respawn_shell_on_exit = false;
        }
        true
    }
}

fn gateway_resume_failure_history(
    route: &crate::terminal::GatewayAgentRoute,
    error: &str,
) -> String {
    format!(
        "\r\n\x1b[1;31mGoWild did not resume {}.\x1b[0m\r\nGateway: {}  Model: {}\r\n{}\r\nFix the gateway settings, then restart GoWild to retry this saved session.\r\n\r\n",
        sanitized_resume_text(&route.cli),
        sanitized_resume_text(&route.gateway_id),
        sanitized_resume_text(&route.model),
        sanitized_resume_text(error),
    )
}

fn sanitized_resume_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

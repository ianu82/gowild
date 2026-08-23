#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CodingAgentLaunchField {
    #[default]
    Cli,
    Gateway,
    Model,
}

impl CodingAgentLaunchField {
    pub(crate) const ALL: [Self; 3] = [Self::Cli, Self::Gateway, Self::Model];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodingAgentLaunchState {
    pub(crate) selected_field: CodingAgentLaunchField,
    pub(crate) cli: crate::cli_adapter::CodingCli,
    pub(crate) gateway_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) error: Option<String>,
}

impl CodingAgentLaunchState {
    pub(crate) fn new(catalog: &crate::gateway::GatewayCatalog) -> Self {
        Self::new_with_environment(catalog, &super::ProcessEnvironment)
    }

    pub(super) fn new_with_environment(
        catalog: &crate::gateway::GatewayCatalog,
        environment: &dyn crate::cli_adapter::Environment,
    ) -> Self {
        let mut state = Self {
            selected_field: CodingAgentLaunchField::Cli,
            cli: crate::cli_adapter::CodingCli::Codex,
            gateway_id: None,
            model: None,
            error: None,
        };
        state.select_preferred_gateway(catalog);
        state.apply_environment_overrides(catalog, environment);
        state
    }

    pub(crate) fn protocol(&self) -> crate::gateway::GatewayProtocol {
        match self.cli {
            crate::cli_adapter::CodingCli::Codex => {
                crate::gateway::GatewayProtocol::OpenAiResponses
            }
            crate::cli_adapter::CodingCli::Claude => {
                crate::gateway::GatewayProtocol::AnthropicMessages
            }
        }
    }

    pub(crate) fn move_field(&mut self, direction: i8) {
        let current = Self::field_index(self.selected_field);
        let next = if direction.is_negative() {
            current.saturating_sub(1)
        } else {
            (current + 1).min(CodingAgentLaunchField::ALL.len() - 1)
        };
        self.selected_field = CodingAgentLaunchField::ALL[next];
        self.error = None;
    }

    pub(crate) fn cycle_selected(
        &mut self,
        catalog: &crate::gateway::GatewayCatalog,
        direction: i8,
    ) {
        match self.selected_field {
            CodingAgentLaunchField::Cli => self.cycle_cli(catalog, direction),
            CodingAgentLaunchField::Gateway => self.cycle_gateway(catalog, direction),
            CodingAgentLaunchField::Model => self.cycle_model(catalog, direction),
        }
        self.error = None;
    }

    pub(crate) fn gateway<'a>(
        &self,
        catalog: &'a crate::gateway::GatewayCatalog,
    ) -> Option<&'a crate::gateway::Gateway> {
        self.gateway_id
            .as_deref()
            .and_then(|id| catalog.gateways.get(id))
    }

    pub(crate) fn cli_label(&self) -> &'static str {
        match self.cli {
            crate::cli_adapter::CodingCli::Codex => "Codex CLI",
            crate::cli_adapter::CodingCli::Claude => "Claude Code",
        }
    }

    pub(crate) fn validation_error(
        &self,
        catalog: &crate::gateway::GatewayCatalog,
    ) -> Option<String> {
        let Some(gateway_id) = self.gateway_id.as_deref() else {
            return Some(format!(
                "No gateway supports {}. Press s to open gateway settings and configure one.",
                self.protocol().display_name()
            ));
        };
        let Some(gateway) = catalog.gateways.get(gateway_id) else {
            return Some(
                "The selected gateway is unavailable. Press s to open gateway settings and choose another."
                    .to_string(),
            );
        };
        if !gateway.supports(self.protocol()) {
            return Some(format!(
                "{} does not support {}. Press s to open gateway settings and choose a compatible gateway.",
                gateway.display_name,
                self.protocol().display_name()
            ));
        }
        if self.model.as_deref().is_none_or(str::is_empty) {
            return Some(format!(
                "No {} model. t test the gateway in settings (s), then choose a model.",
                self.cli_label()
            ));
        }
        None
    }

    pub(crate) fn can_launch(&self, catalog: &crate::gateway::GatewayCatalog) -> bool {
        self.validation_error(catalog).is_none()
    }

    fn field_index(field: CodingAgentLaunchField) -> usize {
        CodingAgentLaunchField::ALL
            .iter()
            .position(|candidate| *candidate == field)
            .unwrap_or_default()
    }

    fn cycle_cli(&mut self, catalog: &crate::gateway::GatewayCatalog, direction: i8) {
        let clis = [
            crate::cli_adapter::CodingCli::Codex,
            crate::cli_adapter::CodingCli::Claude,
        ];
        let current = clis
            .iter()
            .position(|candidate| *candidate == self.cli)
            .unwrap_or_default();
        let next = cycle_index(current, clis.len(), direction);
        self.cli = clis[next];
        self.select_preferred_gateway(catalog);
    }

    fn cycle_gateway(&mut self, catalog: &crate::gateway::GatewayCatalog, direction: i8) {
        let gateway_ids = self.compatible_gateway_ids(catalog);
        if gateway_ids.is_empty() {
            self.gateway_id = None;
            self.model = None;
            return;
        }
        let current = self
            .gateway_id
            .as_ref()
            .and_then(|selected| gateway_ids.iter().position(|id| id == selected))
            .unwrap_or_default();
        let next = cycle_index(current, gateway_ids.len(), direction);
        self.gateway_id = Some(gateway_ids[next].clone());
        self.select_preferred_model(catalog);
    }

    fn cycle_model(&mut self, catalog: &crate::gateway::GatewayCatalog, direction: i8) {
        let models = self.available_models(catalog);
        if models.is_empty() {
            self.model = None;
            return;
        }
        let current = self
            .model
            .as_ref()
            .and_then(|selected| models.iter().position(|model| model == selected))
            .unwrap_or_default();
        self.model = Some(models[cycle_index(current, models.len(), direction)].clone());
    }

    fn select_preferred_gateway(&mut self, catalog: &crate::gateway::GatewayCatalog) {
        let gateway_ids = self.compatible_gateway_ids(catalog);
        self.gateway_id = catalog
            .default_gateway_id
            .as_ref()
            .filter(|id| gateway_ids.contains(id))
            .cloned()
            .or_else(|| gateway_ids.first().cloned());
        self.select_preferred_model(catalog);
    }

    fn select_preferred_model(&mut self, catalog: &crate::gateway::GatewayCatalog) {
        let gateway = self.gateway(catalog);
        self.model = gateway
            .and_then(|gateway| gateway.default_models.get(self.cli.id()).cloned())
            .or_else(|| self.available_models(catalog).first().cloned());
    }

    fn apply_environment_overrides(
        &mut self,
        catalog: &crate::gateway::GatewayCatalog,
        environment: &dyn crate::cli_adapter::Environment,
    ) {
        if let Some(gateway_id) = environment
            .get("GOWILD_GATEWAY")
            .filter(|value| !value.trim().is_empty())
        {
            let gateway_id = gateway_id.trim().to_string();
            match catalog.gateways.get(&gateway_id) {
                Some(gateway) if gateway.supports(self.protocol()) => {
                    self.gateway_id = Some(gateway_id);
                    self.select_preferred_model(catalog);
                }
                Some(_) => {
                    self.gateway_id = Some(gateway_id.clone());
                    self.model = None;
                    self.error = Some(format!(
                        "GOWILD_GATEWAY `{gateway_id}` does not support {}.",
                        self.protocol().display_name()
                    ));
                }
                None => {
                    self.gateway_id = Some(gateway_id.clone());
                    self.model = None;
                    self.error = Some(format!("GOWILD_GATEWAY `{gateway_id}` is not configured."));
                }
            }
        }

        if let Some(model) = environment
            .get("GOWILD_MODEL")
            .filter(|value| !value.trim().is_empty())
        {
            let model = model.trim();
            if model.len() > 256 || model.chars().any(char::is_control) {
                self.model = None;
                self.error = Some("GOWILD_MODEL is invalid.".into());
            } else {
                self.model = Some(model.to_string());
            }
        }
    }

    fn compatible_gateway_ids(&self, catalog: &crate::gateway::GatewayCatalog) -> Vec<String> {
        catalog
            .gateways
            .iter()
            .filter(|(_, gateway)| gateway.supports(self.protocol()))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn available_models(&self, catalog: &crate::gateway::GatewayCatalog) -> Vec<String> {
        let Some(gateway) = self.gateway(catalog) else {
            return Vec::new();
        };
        let mut models = Vec::new();
        if let Some(default_model) = gateway.default_models.get(self.cli.id()) {
            models.push(default_model.clone());
        }
        for model in &gateway.model_discovery.cached_models {
            if model.enabled && !model.embedding && !models.contains(&model.id) {
                models.push(model.id.clone());
            }
        }
        models
    }
}

fn cycle_index(current: usize, len: usize, direction: i8) -> usize {
    if len == 0 {
        return 0;
    }
    if direction.is_negative() {
        current.checked_sub(1).unwrap_or(len - 1)
    } else {
        (current + 1) % len
    }
}

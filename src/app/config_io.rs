use super::App;

impl App {
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                crate::logging::config_write_failed(&path, error_context, &err.to_string());
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return false;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            crate::logging::config_write_failed(&path, error_context, &err.to_string());
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        true
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    pub(super) fn mark_onboarding_started(&mut self) {
        self.update_config_file("onboarding progress", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", true)
        });
    }

    pub(super) fn save_theme_choice(&mut self, choice: crate::app::state::ThemeChoice) {
        let saved = match choice {
            crate::app::state::ThemeChoice::Manual(name) => {
                self.update_config_file("theme", |content| {
                    let content = crate::config::upsert_section_value(
                        content,
                        "theme",
                        "name",
                        &format!("\"{name}\""),
                    );
                    crate::config::upsert_section_bool(&content, "theme", "auto_switch", false)
                })
            }
            crate::app::state::ThemeChoice::FollowTerminal => self
                .update_config_file("theme", |content| {
                    crate::config::upsert_section_bool(content, "theme", "auto_switch", true)
                }),
        };
        if saved {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_status_indicators(&mut self, style: crate::config::StatusIndicatorStyle) {
        if self.update_config_file("status indicators", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        if self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        let value = match delivery {
            crate::config::ToastDelivery::Off => "\"off\"",
            crate::config::ToastDelivery::GoWild => "\"gowild\"",
            crate::config::ToastDelivery::Terminal => "\"terminal\"",
            crate::config::ToastDelivery::System => "\"system\"",
        };
        if self.update_config_file("toast setting", |content| {
            let content =
                crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
            crate::config::remove_section_key(&content, "ui.toast", "enabled")
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        if self.update_config_file("agent border labels", |content| {
            crate::config::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_sort(&mut self, sort: crate::app::state::AgentPanelSort) {
        let value = match sort {
            crate::app::state::AgentPanelSort::Spaces => {
                crate::config::AgentPanelSortConfig::Spaces.as_str()
            }
            crate::app::state::AgentPanelSort::Priority => {
                crate::config::AgentPanelSortConfig::Priority.as_str()
            }
        };
        if self.update_config_file("agent panel sort", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "agent_panel_sort",
                &format!("\"{value}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::state::{ThemeChoice, THEME_CHOICES},
        config::Config,
    };

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "gowild-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    fn app_for_theme_config() -> App {
        let config = Config::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    #[test]
    fn theme_choice_persistence_preserves_pair_and_toggles_follow_mode() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = temp_config_path("theme-choice-persistence");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[theme]\nname = \"cowork\"\ndark_name = \"nord\"\nlight_name = \"one-light\"\nauto_switch = false\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = app_for_theme_config();

        app.save_theme_choice(ThemeChoice::FollowTerminal);

        let followed = std::fs::read_to_string(&path).unwrap();
        let followed: Config = toml::from_str(&followed).unwrap();
        assert!(followed.theme.auto_switch);
        assert_eq!(followed.theme.dark_name.as_deref(), Some("nord"));
        assert_eq!(followed.theme.light_name.as_deref(), Some("one-light"));
        assert_eq!(
            THEME_CHOICES[app.state.settings.theme_choice_selected],
            ThemeChoice::FollowTerminal
        );

        app.save_theme_choice(ThemeChoice::Manual("cowork-light"));

        let manual = std::fs::read_to_string(&path).unwrap();
        let manual: Config = toml::from_str(&manual).unwrap();
        assert!(!manual.theme.auto_switch);
        assert_eq!(manual.theme.name.as_deref(), Some("cowork-light"));
        assert_eq!(manual.theme.dark_name.as_deref(), Some("nord"));
        assert_eq!(manual.theme.light_name.as_deref(), Some("one-light"));
        assert_eq!(app.state.theme_name, "cowork-light");
        assert_eq!(
            THEME_CHOICES[app.state.settings.theme_choice_selected],
            ThemeChoice::Manual("cowork-light")
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

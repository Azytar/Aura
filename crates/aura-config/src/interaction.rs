use super::{ConfigError, ConfigType, validate_section};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneralConfig {
    pub live_config_reload: bool,
    pub import: Vec<String>,
    pub sync_timeout_ms: u64,
}
impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            live_config_reload: true,
            import: Vec::new(),
            sync_timeout_ms: 150,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseConfig {
    pub hide_when_typing: bool,
    pub scroll_lines: usize,
}
impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            hide_when_typing: true,
            scroll_lines: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellConfig {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingAction {
    Copy,
    Paste,
    PasteSelection,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    ReloadConfig,
    None,
    Send(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: String,
    pub mods: u8,
    /// 0: any screen, 1: main only, 2: alternate only.
    pub screen: u8,
    pub action: BindingAction,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardConfig {
    pub use_default_bindings: bool,
    pub bindings: Vec<KeyBinding>,
}
impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            use_default_bindings: true,
            bindings: Vec::new(),
        }
    }
}

fn section<'a>(root: &'a toml::Table, name: &str) -> Option<&'a toml::Table> {
    root.get(name).and_then(toml::Value::as_table)
}
fn boolean(table: &toml::Table, key: &str, default: bool) -> bool {
    table
        .get(key)
        .and_then(toml::Value::as_bool)
        .unwrap_or(default)
}
fn strings(value: Option<&toml::Value>, name: &'static str) -> Result<Vec<String>, ConfigError> {
    value.map_or(Ok(Vec::new()), |value| {
        value
            .as_array()
            .ok_or_else(|| ConfigError::invalid_type(name, "array"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ConfigError::invalid_type(name, "string array"))
            })
            .collect()
    })
}
pub(super) fn bounded(
    table: &toml::Table,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
    name: &'static str,
) -> Result<u64, ConfigError> {
    match table.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_integer()
            .filter(|n| *n >= 0 && (*n as u64) >= min && (*n as u64) <= max)
            .map(|n| n as u64)
            .ok_or_else(|| {
                ConfigError::invalid_value(name, format!("{key} must be {min}..={max}"))
            }),
    }
}

impl GeneralConfig {
    pub(super) fn parse(root: &toml::Table) -> Result<Self, ConfigError> {
        let Some(table) = section(root, "general") else {
            return Ok(Self::default());
        };
        validate_section(
            table,
            "general",
            &[
                ("live_config_reload", ConfigType::Boolean),
                ("import", ConfigType::Strings),
                ("sync_timeout_ms", ConfigType::Integer),
            ],
        )?;
        Ok(Self {
            live_config_reload: boolean(table, "live_config_reload", true),
            import: strings(table.get("import"), "general.import")?,
            sync_timeout_ms: bounded(table, "sync_timeout_ms", 150, 1, 2000, "general")?,
        })
    }
}
impl MouseConfig {
    pub(super) fn parse(root: &toml::Table) -> Result<Self, ConfigError> {
        let Some(table) = section(root, "mouse") else {
            return Ok(Self::default());
        };
        validate_section(
            table,
            "mouse",
            &[
                ("hide_when_typing", ConfigType::Boolean),
                ("scroll_lines", ConfigType::Integer),
            ],
        )?;
        Ok(Self {
            hide_when_typing: boolean(table, "hide_when_typing", true),
            scroll_lines: bounded(table, "scroll_lines", 3, 1, 100, "mouse")? as usize,
        })
    }
}
impl ShellConfig {
    pub(super) fn parse(root: &toml::Table) -> Result<Self, ConfigError> {
        let Some(table) = section(root, "shell") else {
            return Ok(Self::default());
        };
        validate_section(
            table,
            "shell",
            &[
                ("program", ConfigType::String),
                ("args", ConfigType::Array),
                ("working_directory", ConfigType::String),
            ],
        )?;
        let string = |key| {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        };
        let result = Self {
            program: string("program"),
            args: strings(table.get("args"), "shell.args")?,
            working_directory: string("working_directory"),
        };
        if result
            .program
            .iter()
            .chain(result.args.iter())
            .chain(result.working_directory.iter())
            .any(|v| v.contains('\0'))
        {
            return Err(ConfigError::invalid_value(
                "shell",
                "values cannot contain NUL",
            ));
        }
        Ok(result)
    }
}
impl KeyboardConfig {
    pub(super) fn parse(root: &toml::Table) -> Result<Self, ConfigError> {
        let Some(table) = section(root, "keyboard") else {
            return Ok(Self::default());
        };
        validate_section(
            table,
            "keyboard",
            &[
                ("use_default_bindings", ConfigType::Boolean),
                ("bindings", ConfigType::Array),
            ],
        )?;
        let mut bindings = Vec::new();
        for item in table
            .get("bindings")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let table = item
                .as_table()
                .ok_or_else(|| ConfigError::invalid_type("keyboard.bindings", "table array"))?;
            validate_section(
                table,
                "keyboard.bindings",
                &[
                    ("key", ConfigType::String),
                    ("mods", ConfigType::String),
                    ("mode", ConfigType::String),
                    ("action", ConfigType::String),
                    ("chars", ConfigType::String),
                ],
            )?;
            let key = table
                .get("key")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| ConfigError::missing("keyboard.bindings.key"))?
                .to_string();
            let mut mods = 0;
            if let Some(value) = table.get("mods").and_then(toml::Value::as_str) {
                for name in value.split('|').map(str::trim) {
                    mods |= match name {
                        "Shift" => 1,
                        "Alt" => 2,
                        "Control" | "Ctrl" => 4,
                        "Super" => 8,
                        _ => {
                            return Err(ConfigError::invalid_value(
                                "keyboard.bindings.mods",
                                format!("unknown modifier {name}"),
                            ));
                        }
                    };
                }
            }
            let screen = match table
                .get("mode")
                .and_then(toml::Value::as_str)
                .unwrap_or("Any")
            {
                "Any" => 0,
                "Main" => 1,
                "Alternate" => 2,
                _ => {
                    return Err(ConfigError::invalid_value(
                        "keyboard.bindings.mode",
                        "use Any, Main or Alternate",
                    ));
                }
            };
            let action = match (
                table.get("action").and_then(toml::Value::as_str),
                table.get("chars").and_then(toml::Value::as_str),
            ) {
                (None, Some(chars)) => BindingAction::Send(chars.to_string()),
                (Some(action), None) => match action {
                    "Copy" => BindingAction::Copy,
                    "Paste" => BindingAction::Paste,
                    "PasteSelection" => BindingAction::PasteSelection,
                    "IncreaseFontSize" => BindingAction::IncreaseFontSize,
                    "DecreaseFontSize" => BindingAction::DecreaseFontSize,
                    "ResetFontSize" => BindingAction::ResetFontSize,
                    "ScrollPageUp" => BindingAction::ScrollPageUp,
                    "ScrollPageDown" => BindingAction::ScrollPageDown,
                    "ScrollTop" => BindingAction::ScrollTop,
                    "ScrollBottom" => BindingAction::ScrollBottom,
                    "ReloadConfig" => BindingAction::ReloadConfig,
                    "None" => BindingAction::None,
                    _ => {
                        return Err(ConfigError::invalid_value(
                            "keyboard.bindings.action",
                            format!("unknown action {action}"),
                        ));
                    }
                },
                _ => {
                    return Err(ConfigError::invalid_value(
                        "keyboard.bindings",
                        "set exactly one of action or chars",
                    ));
                }
            };
            bindings.push(KeyBinding {
                key,
                mods,
                screen,
                action,
            });
        }
        Ok(Self {
            use_default_bindings: boolean(table, "use_default_bindings", true),
            bindings,
        })
    }
}

pub(super) fn environment(root: &toml::Table) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut result = BTreeMap::new();
    if let Some(table) = section(root, "env") {
        for (key, value) in table {
            if key.is_empty()
                || !key.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
            {
                return Err(ConfigError::invalid_value("env", "invalid variable name"));
            }
            let value = value
                .as_str()
                .filter(|v| !v.contains('\0'))
                .ok_or_else(|| ConfigError::invalid_type("env", "string without NUL"))?;
            result.insert(key.clone(), value.to_string());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::AppConfig;
    #[test]
    fn interaction_options_reject_silent_typos_and_invalid_actions() {
        for text in [
            "[mouse]\nscroll_lines=0",
            "[general]\nsync_timeout_ms=0",
            "[cursor]\nblink_interval_ms=0",
            "[keyboard]\n[[keyboard.bindings]]\nkey='A'\naction='Typo'",
            "[keyboard]\n[[keyboard.bindings]]\nkey='A'\naction='Copy'\nchars='x'",
            "[shell]\nargs=[1]",
            "[env]\n'1BAD'='value'",
        ] {
            let table = toml::from_str(text).unwrap();
            assert!(AppConfig::load_from_text(&table).is_err(), "{text}");
        }
        let table=toml::from_str("[mouse]\nhide_when_typing=false\nscroll_lines=5\n[shell]\nprogram='/bin/sh'\nargs=['-c','']\n[env]\nAURA_TEST='ok'\n").unwrap();
        let config = AppConfig::load_from_text(&table).unwrap();
        assert!(!config.mouse.hide_when_typing);
        assert_eq!(config.mouse.scroll_lines, 5);
        assert_eq!(config.shell.args, ["-c", ""]);
        assert_eq!(config.env.get("AURA_TEST").unwrap(), "ok");
    }
}

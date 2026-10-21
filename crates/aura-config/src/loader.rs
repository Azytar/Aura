use super::ConfigError;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Imports are relative to their containing file, with later tables overriding
/// earlier values. Arrays are replaced as units; cycles and depth are bounded.
pub(super) fn load(path: &Path) -> Result<(toml::Table, Vec<PathBuf>), ConfigError> {
    let mut sources = Vec::new();
    let table = read(path, &mut HashSet::new(), &mut sources, 0)?;
    Ok((table, sources))
}
fn read(
    path: &Path,
    active: &mut HashSet<PathBuf>,
    sources: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<toml::Table, ConfigError> {
    if depth >= 16 {
        return Err(ConfigError::invalid_value(
            "general.import",
            "import nesting exceeds 16 files",
        ));
    }
    let path = path
        .canonicalize()
        .map_err(|e| ConfigError::io(path.display().to_string(), e.to_string()))?;
    if !active.insert(path.clone()) {
        return Err(ConfigError::invalid_value(
            "general.import",
            format!("import cycle at {}", path.display()),
        ));
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| ConfigError::io(path.display().to_string(), e.to_string()))?;
    let table: toml::Table = toml::from_str(&contents)
        .map_err(|e| ConfigError::parse(path.display().to_string(), e.to_string()))?;
    // Validate each source so a later override cannot hide a misspelled key.
    super::AppConfig::load_from_text(&table)?;
    let imports = table
        .get("general")
        .and_then(toml::Value::as_table)
        .and_then(|v| v.get("import"))
        .and_then(toml::Value::as_array);
    let mut combined = toml::Table::new();
    for value in imports.into_iter().flatten() {
        let text = value.as_str().expect("validated import string");
        let import = expand_home(text)?;
        let import = if import.is_absolute() {
            import
        } else {
            path.parent().expect("canonical parent").join(import)
        };
        merge(&mut combined, read(&import, active, sources, depth + 1)?);
    }
    merge(&mut combined, table);
    active.remove(&path);
    sources.push(path);
    Ok(combined)
}
fn merge(target: &mut toml::Table, source: toml::Table) {
    for (key, value) in source {
        if let (Some(toml::Value::Table(old)), toml::Value::Table(new)) =
            (target.get_mut(&key), &value)
        {
            merge(old, new.clone());
        } else {
            target.insert(key, value);
        }
    }
}
pub fn expand_home(value: &str) -> Result<PathBuf, ConfigError> {
    if let Some(relative) = value.strip_prefix("~/") {
        Ok(dirs::home_dir()
            .ok_or_else(|| ConfigError::invalid_value("path", "home directory unavailable"))?
            .join(relative))
    } else {
        Ok(PathBuf::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_imports_merge_tables_and_reject_cycles_and_hidden_typos() {
        let root = std::env::temp_dir().join(format!("aura-import-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let child = root.join("theme.toml");
        let main = root.join("config.toml");
        std::fs::write(
            &child,
            "[colors]\nforeground='#123456'\nbackground='#010203'\n",
        )
        .unwrap();
        std::fs::write(
            &main,
            "[general]\nimport=['theme.toml']\n[colors]\nbackground='#abcdef'\n",
        )
        .unwrap();
        let (config, sources) = super::super::AppConfig::load_with_sources(&main).unwrap();
        assert_eq!(config.colors.palette().foreground, 0x123456);
        assert_eq!(config.colors.palette().background, 0xabcdef);
        assert_eq!(sources.len(), 2);
        std::fs::write(&child, "[general]\nimport=['config.toml']\n").unwrap();
        assert!(load(&main).is_err());
        std::fs::write(&child, "[mouse]\nscroll_linse=4\n").unwrap();
        assert!(load(&main).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! Configuration model deserialized from TOML.
//!
//! The top-level [`ForgeclawConfig`] struct maps to a `forgeclaw.toml` file.
//! See `examples/forgeclaw.example.toml` for a complete sample.
//!
//! ## Layered loading
//!
//! Configuration supports layered precedence (env > project > user):
//!
//! 1. `~/.config/forgeclaw/config.toml` — user-level defaults
//! 2. `./forgeclaw.toml` — project/deployment config
//! 3. Environment variables — `FORGECLAW_*` prefix overrides
//!
//! Use [`ForgeclawConfig::load_layered`] to load with this precedence.

mod merge;
pub mod types;

// We use `BTreeMap` rather than `HashMap` for all config maps so that
// serialized output (TOML, YAML snapshots, debug prints) is deterministic
// and diffable.  This is an intentional deviation from the spec snippets
// which show `HashMap`.
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
pub use types::*;

/// Maximum config file size (1 MiB). Files larger than this are rejected
/// before reading to avoid unnecessary memory pressure.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Default project-level config filename.
const DEFAULT_PROJECT_CONFIG: &str = "forgeclaw.toml";

/// Top-level Forgeclaw configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeclawConfig {
    /// Runtime settings (data directory, concurrency limits, etc.).
    pub runtime: RuntimeConfig,
    /// Database / persistence backend.
    pub store: StoreConfig,
    /// Named provider configurations (keyed by provider name).
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Shared budget pools (keyed by pool name).
    #[serde(default)]
    pub budget_pools: BTreeMap<String, BudgetPoolConfig>,
    /// Named group configurations (keyed by group name).
    pub groups: BTreeMap<String, GroupConfig>,
    /// Named channel configurations (keyed by channel name).
    ///
    /// Every group must reference a channel defined here via its `channel`
    /// field. At least one channel is required for a valid deployment.
    pub channels: BTreeMap<String, ChannelConfig>,
    /// Default container settings.
    pub container: ContainerDefaults,
    /// Optional Tanren integration settings.
    pub tanren: Option<TanrenConfig>,
}

impl ForgeclawConfig {
    /// Load, parse, and validate a configuration file from the given path.
    ///
    /// This is the recommended single-file entry point. It reads the file,
    /// deserializes it, and runs cross-reference validation before returning.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] on I/O, parse, or validation failures.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = read_guarded(path)?;
        let config: Self = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Load configuration with layered precedence: user < project < env.
    ///
    /// 1. Reads `~/.config/forgeclaw/config.toml` (if it exists) as the base.
    /// 2. Merges the project-level config on top. When `project_path` is
    ///    `None`, defaults to `./forgeclaw.toml` in the current directory.
    /// 3. Applies environment variable overrides with the `FORGECLAW_` prefix.
    ///    Env vars use double-underscore (`__`) as a section separator, e.g.
    ///    `FORGECLAW_RUNTIME__LOG_LEVEL=debug` sets `runtime.log_level`.
    ///
    /// After merging, cross-reference validation is run automatically.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] on I/O, parse, or validation failures.
    pub fn load_layered(project_path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut merged = toml::Value::Table(toml::map::Map::new());

        // Layer 1: user-level defaults.
        if let Some(home) = home_config_path() {
            if home.exists() {
                let user_toml = read_toml(&home)?;
                merged = merge::deep_merge(merged, user_toml);
            }
        }

        // Layer 2: project-level config.
        let default_project = std::path::PathBuf::from(DEFAULT_PROJECT_CONFIG);
        let project = project_path.unwrap_or(&default_project);
        if project.exists() {
            let project_toml = read_toml(project)?;
            merged = merge::deep_merge(merged, project_toml);
        }

        // Layer 3: environment variable overrides.
        merged = apply_env_overrides(merged, std::env::vars());

        let config: Self = merged.try_into()?;
        config.validate()?;
        Ok(config)
    }

    /// Validate cross-references between config sections.
    ///
    /// Checks that group references to providers, channels, and budget pools
    /// actually point to entries that exist in the config.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Validation`] listing all broken references.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();

        if self.channels.is_empty() && !self.groups.is_empty() {
            errors.push("at least one channel must be defined when groups are present".to_owned());
        }

        for (group_name, group) in &self.groups {
            if !self.providers.contains_key(&group.provider) {
                errors.push(format!(
                    "group '{group_name}' references unknown provider '{}'",
                    group.provider
                ));
            }
            if !self.channels.contains_key(&group.channel) {
                errors.push(format!(
                    "group '{group_name}' references unknown channel '{}'",
                    group.channel
                ));
            }
            if let Some(pool) = &group.budget_pool {
                if !self.budget_pools.contains_key(pool) {
                    errors.push(format!(
                        "group '{group_name}' references unknown budget_pool '{pool}'"
                    ));
                }
            }
            for (i, fb) in group.fallback.iter().enumerate() {
                if !self.providers.contains_key(&fb.provider) {
                    errors.push(format!(
                        "group '{group_name}' fallback[{i}] references unknown provider '{}'",
                        fb.provider
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Validation { errors })
        }
    }
}

/// Resolve the user-level config path: `~/.config/forgeclaw/config.toml`.
fn home_config_path() -> Option<std::path::PathBuf> {
    dirs_path().map(|p| p.join("forgeclaw").join("config.toml"))
}

/// Platform-appropriate config directory (`~/.config` on Unix).
fn dirs_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
}

/// Read a file with a size guard, rejecting files larger than [`MAX_CONFIG_SIZE`].
fn read_guarded(path: &Path) -> Result<String, ConfigError> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_CONFIG_SIZE {
        return Err(ConfigError::Validation {
            errors: vec![format!(
                "config file {} exceeds maximum size of {} bytes",
                path.display(),
                MAX_CONFIG_SIZE
            )],
        });
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Read a file (with size guard) and parse it as a TOML [`toml::Value`].
fn read_toml(path: &Path) -> Result<toml::Value, ConfigError> {
    let contents = read_guarded(path)?;
    let value: toml::Value = toml::from_str(&contents)?;
    Ok(value)
}

/// Apply `FORGECLAW_*` environment variables as config overrides.
///
/// Only variables that contain `__` (double-underscore) are treated as config
/// overrides, where `__` is the TOML section separator. For example,
/// `FORGECLAW_RUNTIME__LOG_LEVEL=debug` sets `runtime.log_level`.
///
/// Variables without `__` (e.g. `FORGECLAW_ANTHROPIC_API_KEY`) are treated
/// as secrets and are **not** injected into the config. Secrets are handled
/// separately at runtime, never through the config model.
///
/// Values are parsed as TOML scalars first (preserving integers, booleans,
/// floats, etc.) and fall back to a plain string if TOML parsing fails.
///
/// The `env_vars` parameter accepts any iterator of `(String, String)` pairs,
/// allowing tests to inject variables without touching the real environment.
/// Top-level config sections whose keys are maps (provider/group/channel
/// names) rather than static field names.  The second path segment in these
/// sections is a dynamic map key and must preserve its original casing.
const MAP_SECTIONS: &[&str] = &["providers", "groups", "channels", "budget_pools"];

fn apply_env_overrides(
    mut base: toml::Value,
    env_vars: impl Iterator<Item = (String, String)>,
) -> toml::Value {
    let prefix = "FORGECLAW_";
    for (key, value) in env_vars {
        if let Some(suffix) = key.strip_prefix(prefix) {
            // Only process vars with __ path separator — flat vars are secrets.
            if !suffix.contains("__") {
                continue;
            }
            let raw_parts: Vec<&str> = suffix.split("__").collect();
            // Normalize casing: lowercase static section/field names but
            // preserve dynamic map key segments to avoid rewriting
            // case-sensitive provider/group/channel names.
            let parts: Vec<String> = normalize_env_path(&raw_parts);
            let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
            let typed_value = parse_env_value(&value);
            set_nested(&mut base, &refs, typed_value);
        }
    }
    base
}

/// Normalize an env-var path, lowercasing static segments while preserving
/// casing on dynamic map-key segments.
///
/// For a path like `["PROVIDERS", "MyProvider", "BASE_URL"]`:
/// - segment 0 (`PROVIDERS`) is a top-level section name → lowercase
/// - segment 1 (`MyProvider`) is a map key in a `MAP_SECTIONS` entry → preserve
/// - segment 2 (`BASE_URL`) is a field name → lowercase
fn normalize_env_path(parts: &[&str]) -> Vec<String> {
    parts
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let is_map_key = i == 1
                && parts
                    .first()
                    .is_some_and(|s| MAP_SECTIONS.contains(&s.to_lowercase().as_str()));
            if is_map_key {
                (*seg).to_owned()
            } else {
                seg.to_lowercase()
            }
        })
        .collect()
}

/// Parse an environment variable value as a typed TOML value.
///
/// Tries to interpret the string as a TOML scalar (integer, float, boolean,
/// array, inline table) first. Falls back to a plain string if parsing fails.
fn parse_env_value(raw: &str) -> toml::Value {
    // Wrap in a dummy key so toml can parse it as a key-value pair.
    let probe = format!("v = {raw}");
    if let Ok(table) = toml::from_str::<toml::Value>(&probe) {
        if let Some(v) = table.get("v") {
            return v.clone();
        }
    }
    toml::Value::String(raw.to_owned())
}

/// Set a value at a nested TOML path, creating intermediate tables as needed.
fn set_nested(root: &mut toml::Value, path: &[&str], value: toml::Value) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        if let Some(table) = root.as_table_mut() {
            table.insert(path[0].to_owned(), value);
        }
        return;
    }
    if let Some(table) = root.as_table_mut() {
        let child = table
            .entry(path[0])
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        set_nested(child, &path[1..], value);
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_env;
#[cfg(test)]
mod tests_validation;

//! TOML lock file data structures for hive-lock.toml

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::Key;

/// Schema version for the TOML lock file format
pub const SCHEMA_VERSION: &str = "1.0";

/// Complete TOML lock file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TomlLockFile {
    pub schema_version: String,
    pub meta: TomlMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defaults: Option<TomlDefaults>,
    pub nodes: HashMap<String, TomlNode>,
}

/// Metadata about the lock file generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TomlMeta {
    pub generated_from: String,
    pub generated_at: String,
    pub flake_lock_hash: String,
    pub generator_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_apply_all: Option<bool>,
}

/// Default deployment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TomlDefaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<TomlDeployment>,
}

/// Per-node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TomlNode {
    /// Path to the system derivation
    pub system_drv: String,

    /// Path to the built system (if --build was used)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_config: Option<String>,

    /// Flake attribute path for this node
    pub flake_attr: String,

    /// Deployment configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<TomlDeployment>,
}

/// Deployment configuration for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TomlDeployment {
    #[serde(rename = "targetHost", skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,

    #[serde(rename = "targetUser", skip_serializing_if = "Option::is_none")]
    pub target_user: Option<String>,

    #[serde(rename = "targetPort", skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    #[serde(rename = "buildOnTarget", skip_serializing_if = "Option::is_none")]
    pub build_on_target: Option<bool>,

    #[serde(
        rename = "allowLocalDeployment",
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_local_deployment: Option<bool>,

    #[serde(
        rename = "replaceUnknownProfiles",
        skip_serializing_if = "Option::is_none"
    )]
    pub replace_unknown_profiles: Option<bool>,

    #[serde(
        rename = "privilegeEscalationCommand",
        skip_serializing_if = "Option::is_none"
    )]
    pub privilege_escalation_command: Option<Vec<String>>,

    #[serde(rename = "sshOptions", skip_serializing_if = "Option::is_none")]
    pub extra_ssh_options: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, Key>>,
}

impl TomlLockFile {
    /// Create a new lock file with basic metadata
    pub fn new(
        generated_from: String,
        generated_at: String,
        flake_lock_hash: String,
        generator_version: String,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            meta: TomlMeta {
                generated_from,
                generated_at,
                flake_lock_hash,
                generator_version,
                allow_apply_all: None,
            },
            defaults: None,
            nodes: HashMap::new(),
        }
    }

    /// Add a node to the lock file
    pub fn add_node(&mut self, name: String, node: TomlNode) {
        self.nodes.insert(name, node);
    }

    /// Generate the TOML file header comment
    pub fn header_comment(&self) -> String {
        format!(
            "# hive-lock.toml\n\
             # Auto-generated lock file - DO NOT EDIT MANUALLY\n\
             # Generated from: {}\n\
             # Generated at: {}\n\
             # Regenerate with: colmena generate-toml\n\n",
            self.meta.generated_from, self.meta.generated_at
        )
    }
}

impl TomlNode {
    /// Create a new node with required fields
    pub fn new(system_drv: String, flake_attr: String) -> Self {
        Self {
            system_drv,
            system_config: None,
            flake_attr,
            deployment: None,
        }
    }
}

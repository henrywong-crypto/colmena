//! Lock file loading for fast deployments

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::error::{ColmenaError, ColmenaResult};
use crate::nix::{NodeName, ProfileDerivation, StoreDerivation, TomlLockFile};

/// Load derivation paths from hive-lock.toml if it exists
pub fn load_lock_file(
    lock_file_path: &Path,
    nodes: &[NodeName],
) -> ColmenaResult<Option<HashMap<NodeName, ProfileDerivation>>> {
    if !lock_file_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(lock_file_path)?;
    let lock_file: TomlLockFile = toml::from_str(&contents).map_err(|e| ColmenaError::Unknown {
        message: format!("Failed to parse {}: {}", lock_file_path.display(), e),
    })?;

    let mut result = HashMap::new();

    for node_name in nodes {
        if let Some(node_config) = lock_file.nodes.get(node_name.as_str()) {
            // Parse the derivation path
            let store_path = crate::nix::StorePath::try_from(node_config.system_drv.clone())?;
            let drv = StoreDerivation::from_store_path_unchecked(store_path);
            result.insert(node_name.clone(), drv);
        } else {
            // Node not found in lock file, need to evaluate
            return Ok(None);
        }
    }

    Ok(Some(result))
}

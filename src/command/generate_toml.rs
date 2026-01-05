//! Generate TOML lock file from Nix flake evaluations

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use clap::Args;
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::error::{ColmenaError, ColmenaResult};
use crate::nix::{Flake, NodeFilter, TomlDeployment, TomlLockFile, TomlNode};

const COLMENA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Generate TOML lock file from Nix flake evaluations
#[derive(Debug, Args)]
#[command(
    name = "generate-toml",
    about = "Generate TOML lock file from Nix flake evaluations"
)]
pub struct Opts {
    /// Output lock file path
    #[arg(short = 'o', long, default_value = "hive-lock.toml")]
    output: PathBuf,

    /// Generate only for specific nodes
    #[arg(long)]
    on: Option<NodeFilter>,

    /// Number of parallel evaluations
    #[arg(long, default_value = "4")]
    parallel: usize,

    /// Build systems and include built paths
    #[arg(long)]
    build: bool,

    /// Merge with deployment config template
    #[arg(long)]
    template: Option<PathBuf>,

    /// Show what would be generated without writing
    #[arg(long)]
    dry_run: bool,

    /// Continue even if some nodes fail to evaluate
    #[arg(long)]
    keep_going: bool,
}

/// Result of evaluating a single node
struct EvaluationResult {
    node_name: String,
    drv_path: String,
    system_path: Option<String>,
    duration_secs: u64,
}

/// Run the generate-toml command
pub async fn run(flake: Flake, opts: Opts) -> ColmenaResult<()> {
    let start_time = Instant::now();

    // Step 1: Discover configurations
    println!("[1/4] Discovering configurations...");
    let node_names = discover_configurations(&flake).await?;

    if node_names.is_empty() {
        return Err(ColmenaError::Unknown {
            message: "No NixOS configurations found in flake\n\n\
                     Hint: Add nixosConfigurations to your flake.nix:\n\
                     outputs = { nixpkgs, ... }: {\n\
                       nixosConfigurations.my-server = nixpkgs.lib.nixosSystem {\n\
                         modules = [ ./configuration.nix ];\n\
                       };\n\
                     };"
            .to_string(),
        });
    }

    // Filter nodes if requested
    let filtered_nodes = if let Some(filter) = &opts.on {
        filter_nodes(&node_names, filter)?
    } else {
        node_names.clone()
    };

    // Show count and first few nodes
    if node_names.len() <= 10 {
        println!(
            "  ├─ Found {} nixosConfigurations: {}",
            node_names.len(),
            node_names.join(", ")
        );
    } else {
        let preview: Vec<_> = node_names.iter().take(5).cloned().collect();
        println!(
            "  ├─ Found {} nixosConfigurations: {}, ... ({} more)",
            node_names.len(),
            preview.join(", "),
            node_names.len() - 5
        );
    }

    if filtered_nodes.len() < node_names.len() {
        if filtered_nodes.len() <= 10 {
            println!(
                "  └─ Filtered to {} nodes: {}",
                filtered_nodes.len(),
                filtered_nodes.join(", ")
            );
        } else {
            let preview: Vec<_> = filtered_nodes.iter().take(5).cloned().collect();
            println!(
                "  └─ Filtered to {} nodes: {}, ... ({} more)",
                filtered_nodes.len(),
                preview.join(", "),
                filtered_nodes.len() - 5
            );
        }
    }

    // Step 2: Evaluate configurations in parallel
    println!("[2/4] Evaluating configurations...");
    let eval_results = evaluate_nodes_parallel(
        &flake,
        &filtered_nodes,
        opts.parallel,
        opts.build,
        opts.keep_going,
    )?;

    for result in &eval_results {
        println!("  ├─ {}: {}s", result.node_name, result.duration_secs);
        println!("      └─ {}", result.drv_path);
    }

    // Step 3: Extract deployment metadata (if colmena output exists)
    println!("[3/4] Extracting deployment metadata...");
    let has_colmena = check_colmena_output(&flake).await?;

    let mut deployments: HashMap<String, TomlDeployment> = HashMap::new();
    if has_colmena {
        for node_name in &filtered_nodes {
            if let Some(deployment) = extract_deployment_metadata().await? {
                deployments.insert(node_name.clone(), deployment);
                println!("  ├─ {}: found in colmena output", node_name);
            }
        }
    }

    // Step 4: Generate TOML lock file
    println!("[4/4] Generating hive-lock.toml...");

    let lock_file = build_lock_file(eval_results, deployments).await?;

    // Merge with template if provided
    let final_lock_file = if let Some(template_path) = &opts.template {
        println!("  ├─ Merging with template: {}", template_path.display());
        merge_with_template(lock_file, template_path)?
    } else {
        lock_file
    };

    if opts.dry_run {
        println!("  └─ Dry run - not writing file");
        let toml_content = serialize_lock_file(&final_lock_file)?;
        println!("\n{}", toml_content);
    } else {
        write_lock_file(&opts.output, &final_lock_file)?;
        println!("  ├─ Added {} nodes", final_lock_file.nodes.len());
        println!("  └─ Written to {}", opts.output.display());
    }

    let elapsed = start_time.elapsed();
    println!("\n✓ Generation completed in {}s", elapsed.as_secs());
    println!("  └─ Evaluated {} nodes", filtered_nodes.len());

    Ok(())
}

/// Discover NixOS configurations in the flake
async fn discover_configurations(flake: &Flake) -> ColmenaResult<Vec<String>> {
    // Use nix eval to get attribute names without evaluating the configurations
    // Query the nixosConfigurations attribute directly
    let flake_attr = format!("{}#nixosConfigurations", flake.uri());

    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--apply",
            "configs: builtins.attrNames configs",
            &flake_attr,
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ColmenaError::Unknown {
            message: format!("Failed to discover nixosConfigurations:\n{}", stderr),
        });
    }

    let mut configs: Vec<String> =
        serde_json::from_slice(&output.stdout).map_err(|e| ColmenaError::Unknown {
            message: format!("Failed to parse configuration names: {}", e),
        })?;

    configs.sort();
    Ok(configs)
}

/// Filter nodes based on node filter
fn filter_nodes(all_nodes: &[String], filter: &NodeFilter) -> ColmenaResult<Vec<String>> {
    use crate::nix::NodeName;

    // Convert strings to NodeNames
    let node_names: Vec<NodeName> = all_nodes
        .iter()
        .filter_map(|name| NodeName::new(name.clone()).ok())
        .collect();

    // Use existing filter logic
    let filtered_set = filter.filter_node_names(&node_names)?;

    if filtered_set.is_empty() {
        return Err(ColmenaError::Unknown {
            message: format!(
                "No nodes matched the filter. Available nodes: {}",
                all_nodes.join(", ")
            ),
        });
    }

    // Convert back to strings
    let filtered: Vec<String> = filtered_set
        .into_iter()
        .map(|name| name.to_string())
        .collect();

    Ok(filtered)
}

/// Evaluate multiple nodes in parallel
fn evaluate_nodes_parallel(
    flake: &Flake,
    nodes: &[String],
    parallel: usize,
    build: bool,
    keep_going: bool,
) -> ColmenaResult<Vec<EvaluationResult>> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel)
        .build()
        .map_err(|e| ColmenaError::Unknown {
            message: format!("Failed to create thread pool: {}", e),
        })?;

    let total = nodes.len();
    let completed = Arc::new(AtomicUsize::new(0));

    let results: Vec<_> = pool.install(|| {
        nodes
            .par_iter()
            .map(|node_name| {
                let result = evaluate_single_node(flake, node_name, build);
                let count = completed.fetch_add(1, Ordering::SeqCst) + 1;

                match &result {
                    Ok(_) => {
                        println!("  ├─ [{}/{}] {}: ✓", count, total, node_name);
                    }
                    Err(e) => {
                        println!("  ├─ [{}/{}] {}: ✗ ({})", count, total, node_name, e);
                    }
                }

                (node_name.clone(), result)
            })
            .collect()
    });

    if keep_going {
        // Filter out failures and continue
        let successes: Vec<_> = results
            .into_iter()
            .filter_map(|(_name, result)| result.ok())
            .collect();

        let failed_count = total - successes.len();
        if failed_count > 0 {
            println!(
                "  └─ ⚠️  {} node(s) failed, continuing with {} successful nodes",
                failed_count,
                successes.len()
            );
        }

        Ok(successes)
    } else {
        // Stop on first error
        results
            .into_iter()
            .map(|(_, result)| result)
            .collect::<ColmenaResult<Vec<_>>>()
    }
}

/// Evaluate a single node to get its derivation path
fn evaluate_single_node(
    flake: &Flake,
    node_name: &str,
    build: bool,
) -> ColmenaResult<EvaluationResult> {
    let start = Instant::now();

    // Evaluate the derivation path
    let drv_attr = format!(
        "{}#nixosConfigurations.{}.config.system.build.toplevel.drvPath",
        flake.uri(),
        node_name
    );

    let output = Command::new("nix")
        .args(["eval", &drv_attr, "--json"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ColmenaError::Unknown {
            message: format!("Failed to evaluate node '{}':\n{}", node_name, stderr),
        });
    }

    let drv_path: String =
        serde_json::from_slice(&output.stdout).map_err(|e| ColmenaError::Unknown {
            message: format!("Failed to parse derivation path JSON: {}", e),
        })?;

    // Validate derivation path
    validate_drv_path(&drv_path)?;

    // Optionally build the system
    let system_path = if build {
        Some(build_system(flake, node_name)?)
    } else {
        None
    };

    let duration = start.elapsed();

    Ok(EvaluationResult {
        node_name: node_name.to_string(),
        drv_path,
        system_path,
        duration_secs: duration.as_secs(),
    })
}

/// Build a system and return its store path
fn build_system(flake: &Flake, node_name: &str) -> ColmenaResult<String> {
    let attr = format!(
        "{}#nixosConfigurations.{}.config.system.build.toplevel",
        flake.uri(),
        node_name
    );

    let output = Command::new("nix")
        .args(["build", &attr, "--no-link", "--print-out-paths"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ColmenaError::Unknown {
            message: format!("Failed to build node '{}':\n{}", node_name, stderr),
        });
    }

    let path = String::from_utf8(output.stdout)
        .map_err(|e| ColmenaError::Unknown {
            message: format!("Failed to parse build output: {}", e),
        })?
        .trim()
        .to_string();

    Ok(path)
}

/// Validate that a derivation path is legitimate
fn validate_drv_path(path: &str) -> ColmenaResult<()> {
    if !path.starts_with("/nix/store/") {
        return Err(ColmenaError::Unknown {
            message: "Invalid derivation path: must be in /nix/store".to_string(),
        });
    }

    if !path.ends_with(".drv") {
        return Err(ColmenaError::Unknown {
            message: "Invalid derivation path: must end with .drv".to_string(),
        });
    }

    if !Path::new(path).exists() {
        return Err(ColmenaError::Unknown {
            message: format!("Derivation path does not exist: {}", path),
        });
    }

    Ok(())
}

/// Check if the flake has a colmena output
async fn check_colmena_output(flake: &Flake) -> ColmenaResult<bool> {
    // Try to evaluate the colmena attribute to see if it exists
    let flake_attr = format!("{}#colmena", flake.uri());

    let output = Command::new("nix")
        .args(["eval", "--apply", "x: true", &flake_attr])
        .output()?;

    // If the command succeeds, colmena output exists
    Ok(output.status.success())
}

/// Extract deployment metadata for a node from colmena output
async fn extract_deployment_metadata() -> ColmenaResult<Option<TomlDeployment>> {
    // TODO: Implement deployment metadata extraction
    // For now, return None as deployment metadata is optional
    Ok(None)
}

/// Build the lock file structure
async fn build_lock_file(
    eval_results: Vec<EvaluationResult>,
    deployments: HashMap<String, TomlDeployment>,
) -> ColmenaResult<TomlLockFile> {
    let flake_lock_hash = hash_flake_lock()?;
    let generated_at = Utc::now().to_rfc3339();

    let mut lock_file = TomlLockFile::new(
        "flake.nix".to_string(),
        generated_at,
        flake_lock_hash,
        COLMENA_VERSION.to_string(),
    );

    for result in eval_results {
        let mut node = TomlNode::new(
            result.drv_path,
            format!("nixosConfigurations.{}", result.node_name),
        );

        node.system_config = result.system_path;
        node.deployment = deployments.get(&result.node_name).cloned();

        lock_file.add_node(result.node_name, node);
    }

    Ok(lock_file)
}

/// Hash the flake.lock file
fn hash_flake_lock() -> ColmenaResult<String> {
    let flake_lock_path = Path::new("flake.lock");

    if !flake_lock_path.exists() {
        return Err(ColmenaError::Unknown {
            message: "flake.lock not found in current directory".to_string(),
        });
    }

    let contents = fs::read(flake_lock_path)?;

    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let hash = hasher.finalize();

    Ok(format!("sha256:{:x}", hash))
}

/// Merge generated lock file with template
fn merge_with_template(
    mut generated: TomlLockFile,
    template_path: &Path,
) -> ColmenaResult<TomlLockFile> {
    let template_content = fs::read_to_string(template_path)?;

    let template: TomlLockFile =
        toml::from_str(&template_content).map_err(|e| ColmenaError::Unknown {
            message: format!("Failed to parse template TOML: {}", e),
        })?;

    // Merge metadata (prefer template for allow_apply_all)
    if let Some(allow_apply_all) = template.meta.allow_apply_all {
        generated.meta.allow_apply_all = Some(allow_apply_all);
    }

    // Merge defaults
    if template.defaults.is_some() {
        generated.defaults = template.defaults;
    }

    // Merge node deployment configs
    for (node_name, template_node) in template.nodes {
        if let Some(generated_node) = generated.nodes.get_mut(&node_name) {
            // Merge deployment config from template
            if template_node.deployment.is_some() {
                generated_node.deployment = template_node.deployment;
            }
        }
    }

    Ok(generated)
}

/// Serialize lock file to TOML string
fn serialize_lock_file(lock_file: &TomlLockFile) -> ColmenaResult<String> {
    let toml_content = toml::to_string_pretty(lock_file).map_err(|e| ColmenaError::Unknown {
        message: format!("Failed to serialize TOML: {}", e),
    })?;

    Ok(format!("{}{}", lock_file.header_comment(), toml_content))
}

/// Write lock file to disk
fn write_lock_file(path: &Path, lock_file: &TomlLockFile) -> ColmenaResult<()> {
    let content = serialize_lock_file(lock_file)?;

    fs::write(path, content)?;

    // Set permissions to 0644 (owner read/write, others read)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o644);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

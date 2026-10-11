//! Runs Terraform for one machine: one working directory and one state per
//! machine, with the module embedded in the app so the app always runs the
//! module it was built and tested with.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{ProviderError, ProviderResult};

/// `infra/terraform/aws`, file by file. A test checks nothing is missing.
pub const AWS_MODULE: &[(&str, &str)] = &[
    (
        ".terraform.lock.hcl",
        include_str!("../../../infra/terraform/aws/.terraform.lock.hcl"),
    ),
    (
        "idle_alarm.tf",
        include_str!("../../../infra/terraform/aws/idle_alarm.tf"),
    ),
    (
        "instance.tf",
        include_str!("../../../infra/terraform/aws/instance.tf"),
    ),
    (
        "library.tf",
        include_str!("../../../infra/terraform/aws/library.tf"),
    ),
    (
        "main.tf",
        include_str!("../../../infra/terraform/aws/main.tf"),
    ),
    (
        "network.tf",
        include_str!("../../../infra/terraform/aws/network.tf"),
    ),
    (
        "outputs.tf",
        include_str!("../../../infra/terraform/aws/outputs.tf"),
    ),
    (
        "templates/user_data.ps1.tftpl",
        include_str!("../../../infra/terraform/aws/templates/user_data.ps1.tftpl"),
    ),
    (
        "variables.tf",
        include_str!("../../../infra/terraform/aws/variables.tf"),
    ),
    (
        "versions.tf",
        include_str!("../../../infra/terraform/aws/versions.tf"),
    ),
];

/// Variables file the app writes next to the module.
pub const VARS_FILE: &str = "terraform.tfvars.json";

/// How long Terraform waits for another device's lock on the same state.
const LOCK_TIMEOUT: &str = "-lock-timeout=5m";

/// Runs the Terraform CLI. Behind a trait so tests can record the calls.
#[async_trait]
pub trait Terraform: Send + Sync {
    /// Runs `terraform <args>` in `dir` with `env` added, returning stdout.
    async fn run(
        &self,
        dir: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> ProviderResult<String>;
}

/// The real `terraform` binary.
pub struct TerraformCli {
    binary: Option<PathBuf>,
    plugin_cache: PathBuf,
}

impl TerraformCli {
    /// Uses `REGA_TERRAFORM` when set, otherwise `terraform` on `PATH`.
    /// Provider plugins are shared between machines through `plugin_cache`.
    pub fn find(plugin_cache: PathBuf) -> Self {
        let binary = std::env::var_os("REGA_TERRAFORM")
            .map(PathBuf::from)
            .or_else(|| find_on_path("terraform"));
        Self {
            binary,
            plugin_cache,
        }
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

#[async_trait]
impl Terraform for TerraformCli {
    async fn run(
        &self,
        dir: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> ProviderResult<String> {
        let binary = self.binary.as_ref().ok_or_else(|| {
            ProviderError::Cloud(
                "Terraform isn't installed: put terraform on PATH or set REGA_TERRAFORM".into(),
            )
        })?;
        tokio::fs::create_dir_all(&self.plugin_cache)
            .await
            .map_err(|e| io_error("create the Terraform plugin cache", e))?;
        let output = tokio::process::Command::new(binary)
            .args(args)
            .current_dir(dir)
            // The credentials in `env` must win over any profile.
            .env_remove("AWS_PROFILE")
            .env_remove("AWS_DEFAULT_PROFILE")
            .env_remove("AWS_SESSION_TOKEN")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env("TF_IN_AUTOMATION", "1")
            .env("TF_INPUT", "0")
            .env("CHECKPOINT_DISABLE", "1")
            .env("TF_PLUGIN_CACHE_DIR", &self.plugin_cache)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| io_error("run Terraform", e))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let verb = args.first().map_or("", String::as_str);
        Err(ProviderError::Cloud(format!(
            "terraform {verb} failed: {}",
            summarize_errors(&stderr)
        )))
    }
}

pub(crate) fn io_error(what: &str, e: std::io::Error) -> ProviderError {
    ProviderError::Cloud(format!("couldn't {what}: {e}"))
}

/// Keeps each `Error:` summary and its explanation, dropping the source
/// locations Terraform prints in between. Falls back to the whole output.
pub fn summarize_errors(stderr: &str) -> String {
    let mut errors = Vec::new();
    for block in stderr.split("Error: ").skip(1) {
        let mut paragraphs = block
            .trim()
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.starts_with("with ") && !p.starts_with("on "));
        let summary = paragraphs.next().unwrap_or_default().replace('\n', " ");
        let detail = paragraphs
            .filter(|p| !p.starts_with('│') && !p.starts_with('╵'))
            .last()
            .map(|p| p.replace('\n', " "));
        errors.push(match detail {
            Some(d) if d != summary => format!("{summary}: {d}"),
            _ => summary,
        });
    }
    if errors.is_empty() {
        stderr.trim().to_string()
    } else {
        errors.join("; ")
    }
}

/// Where a machine's Terraform state lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    pub bucket: String,
    pub key: String,
    pub region: String,
}

pub fn init_args(backend: &Backend) -> Vec<String> {
    vec![
        "init".into(),
        "-input=false".into(),
        "-no-color".into(),
        "-reconfigure".into(),
        format!("-backend-config=bucket={}", backend.bucket),
        format!("-backend-config=key={}", backend.key),
        format!("-backend-config=region={}", backend.region),
    ]
}

pub fn apply_args() -> Vec<String> {
    [
        "apply",
        "-input=false",
        "-no-color",
        "-auto-approve",
        LOCK_TIMEOUT,
    ]
    .map(String::from)
    .to_vec()
}

pub fn destroy_args() -> Vec<String> {
    [
        "destroy",
        "-input=false",
        "-no-color",
        "-auto-approve",
        LOCK_TIMEOUT,
    ]
    .map(String::from)
    .to_vec()
}

pub fn output_args() -> Vec<String> {
    ["output", "-json", "-no-color"].map(String::from).to_vec()
}

pub fn state_pull_args() -> Vec<String> {
    ["state", "pull"].map(String::from).to_vec()
}

/// Writes the module and `vars_json` into `dir`, replacing files from an
/// older app version. `.terraform` (providers, backend) is kept.
pub async fn prepare_workdir(dir: &Path, vars_json: &str) -> ProviderResult<()> {
    let write = |path: PathBuf, contents: String| async move {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_error("create the machine folder", e))?;
        }
        tokio::fs::write(&path, contents)
            .await
            .map_err(|e| io_error("write the Terraform files", e))
    };
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| io_error("create the machine folder", e))?;
    // Drop .tf files the current module no longer has.
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| io_error("read the machine folder", e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| io_error("read the machine folder", e))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tf") && !AWS_MODULE.iter().any(|(f, _)| *f == name) {
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|e| io_error("remove an old Terraform file", e))?;
        }
    }
    for (name, contents) in AWS_MODULE {
        write(dir.join(name), contents.to_string()).await?;
    }
    write(dir.join(VARS_FILE), vars_json.to_string()).await
}

#[derive(Deserialize)]
struct OutputValue {
    value: serde_json::Value,
}

/// `terraform output -json`, reduced to the string outputs that are set.
pub fn parse_outputs(json: &str) -> ProviderResult<HashMap<String, String>> {
    let outputs: HashMap<String, OutputValue> = serde_json::from_str(json)
        .map_err(|e| ProviderError::Cloud(format!("unreadable Terraform outputs: {e}")))?;
    Ok(outputs
        .into_iter()
        .filter_map(|(k, v)| match v.value {
            serde_json::Value::String(s) if !s.is_empty() => Some((k, s)),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_module_file_is_embedded() {
        let module = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/terraform/aws");
        let mut on_disk: Vec<String> = std::fs::read_dir(&module)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tf") || n == ".terraform.lock.hcl")
            .collect();
        for entry in std::fs::read_dir(module.join("templates")).unwrap() {
            on_disk.push(format!(
                "templates/{}",
                entry.unwrap().file_name().to_string_lossy()
            ));
        }
        on_disk.sort();
        let embedded: Vec<String> = AWS_MODULE.iter().map(|(n, _)| n.to_string()).collect();
        assert_eq!(embedded, on_disk);
    }

    #[test]
    fn summarizes_terraform_errors() {
        let stderr = "\
Error: creating EC2 Instance: operation error EC2: RunInstances, https response error StatusCode: 403

  with aws_instance.machine,
  on instance.tf line 6, in resource \"aws_instance\" \"machine\":
   6: resource \"aws_instance\" \"machine\" {

Blocked: This account is currently blocked and not recognized as a valid account.

Error: Invalid value for variable

  on terraform.tfvars.json line 3:

instance_type must be one of t3.micro, t3.medium, t3.large or g4dn.xlarge.
";
        assert_eq!(
            summarize_errors(stderr),
            "creating EC2 Instance: operation error EC2: RunInstances, https response \
             error StatusCode: 403: Blocked: This account is currently blocked and not \
             recognized as a valid account.; Invalid value for variable: instance_type \
             must be one of t3.micro, t3.medium, t3.large or g4dn.xlarge."
        );
        assert_eq!(summarize_errors("plain failure\n"), "plain failure");
    }

    #[test]
    fn backend_goes_into_init() {
        let args = init_args(&Backend {
            bucket: "rega-tfstate-1".into(),
            key: "machines/m.tfstate".into(),
            region: "eu-north-1".into(),
        });
        assert!(args.contains(&"-backend-config=key=machines/m.tfstate".to_string()));
        assert!(args.contains(&"-reconfigure".to_string()));
    }

    #[test]
    fn outputs_keep_set_strings() {
        let json = r#"{
            "instance_id": {"sensitive": false, "type": "string", "value": "i-1"},
            "public_ip": {"sensitive": false, "type": "string", "value": ""},
            "library_volume_id": {"sensitive": false, "type": "string", "value": null}
        }"#;
        let out = parse_outputs(json).unwrap();
        assert_eq!(out.get("instance_id").map(String::as_str), Some("i-1"));
        assert!(!out.contains_key("public_ip"));
        assert!(!out.contains_key("library_volume_id"));
    }

    #[tokio::test]
    async fn workdir_gets_the_module_and_drops_old_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("removed.tf"), "").unwrap();
        std::fs::create_dir(dir.path().join(".terraform")).unwrap();
        prepare_workdir(dir.path(), "{}").await.unwrap();
        assert!(!dir.path().join("removed.tf").exists());
        assert!(dir.path().join(".terraform").exists());
        assert!(dir.path().join("templates/user_data.ps1.tftpl").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(VARS_FILE)).unwrap(),
            "{}"
        );
    }

    #[tokio::test]
    async fn missing_binary_is_explained() {
        let tf = TerraformCli {
            binary: None,
            plugin_cache: PathBuf::from("/nonexistent"),
        };
        let err = tf.run(Path::new("."), &[], &[]).await.unwrap_err();
        assert!(err.to_string().contains("REGA_TERRAFORM"));
    }
}

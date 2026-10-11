//! Runs Terraform for one machine: one working directory and one state per
//! machine, with the module embedded in the app so the app always runs the
//! module it was built and tested with.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

/// Names the Terraform process working in a machine folder, so an app opened
/// again can tell whether the last run is still going or was killed.
const RUN_MARKER: &str = "terraform.pid";

/// What Terraform may see of the app's environment, besides the credentials
/// the app passes. Anything else, such as `TF_WORKSPACE`, `TF_CLI_ARGS`,
/// `TF_VAR_*` or another AWS profile, could point a run somewhere else.
const PASSTHROUGH_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "LANG",
    "TZ",
    "TMPDIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "AWS_CA_BUNDLE",
];

fn passes_through(name: &str) -> bool {
    PASSTHROUGH_ENV.contains(&name) || name.starts_with("LC_")
}

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
        // Output goes to files, not pipes, and Terraform gets its own process
        // group: if the app quits mid-run, Terraform can still finish and
        // release its state lock instead of dying on a closed pipe.
        // Unique per run: in a Flatpak sandbox the app's pid is always the
        // same, and a run from an earlier sandbox may still be writing here.
        let id = format!("{}-{}", std::process::id(), unique_suffix());
        let out_path = dir.join(format!(".terraform-{id}.out"));
        let err_path = dir.join(format!(".terraform-{id}.err"));
        let create = |path: &Path| {
            std::fs::File::create(path).map_err(|e| io_error("save Terraform's output", e))
        };
        let mut child = tokio::process::Command::new(binary)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(passes_through)))
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env("TF_IN_AUTOMATION", "1")
            .env("TF_INPUT", "0")
            .env("CHECKPOINT_DISABLE", "1")
            .env("TF_PLUGIN_CACHE_DIR", &self.plugin_cache)
            .stdin(Stdio::null())
            .stdout(create(&out_path)?)
            .stderr(create(&err_path)?)
            .process_group(0)
            .spawn()
            .map_err(|e| io_error("run Terraform", e))?;
        if let Some(marker) = child.id().and_then(RunMarker::of) {
            // Best effort: without it a killed run's lock waits out its age.
            let _ = tokio::fs::write(dir.join(RUN_MARKER), marker.to_json()).await;
        }
        let status = child.wait().await;
        let _ = tokio::fs::remove_file(dir.join(RUN_MARKER)).await;
        let read = |path: PathBuf| async move {
            let text = tokio::fs::read(&path).await.unwrap_or_default();
            let _ = tokio::fs::remove_file(&path).await;
            String::from_utf8_lossy(&text).into_owned()
        };
        let (stdout, stderr) = (read(out_path).await, read(err_path).await);
        if status.map_err(|e| io_error("run Terraform", e))?.success() {
            return Ok(stdout);
        }
        let verb = args.first().map_or("", String::as_str);
        Err(ProviderError::Cloud(format!(
            "terraform {verb} failed: {}",
            summarize_errors(&stderr)
        )))
    }
}

/// A Terraform process started in a machine folder, maybe by an app that
/// has quit since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMarker {
    pub pid: u32,
    /// When the process started, in clock ticks since boot. Together with
    /// the pid it names the process even after it `exec`s the real binary
    /// (snap, tfenv and asdf put a launcher in front of Terraform).
    pub start_ticks: u64,
    /// Unix seconds.
    pub started: u64,
    /// The pid namespace `pid` belongs to. Each Flatpak sandbox has its own,
    /// so a run started by an earlier sandbox can't be seen from this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_namespace: Option<String>,
    /// The boot the run started in. No run outlives a reboot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

impl RunMarker {
    /// The marker for running process `pid` (Linux), a child of this app.
    pub fn of(pid: u32) -> Option<Self> {
        Some(Self {
            pid,
            start_ticks: start_ticks(pid)?,
            started: unix_now(),
            pid_namespace: pid_namespace(),
            boot_id: boot_id(),
        })
    }

    /// Whether this app can tell if the run is still going: it can't see
    /// processes in another sandbox that is still up in the same boot. When
    /// the app quits, the Flatpak sandbox stays up until Terraform exits, so
    /// such a run may well be alive, holding its lock.
    fn is_visible(&self) -> bool {
        let other_namespace = matches!(
            (&self.pid_namespace, pid_namespace()),
            (Some(theirs), Some(ours)) if *theirs != ours
        );
        let same_boot = matches!(
            (&self.boot_id, boot_id()),
            (Some(theirs), Some(ours)) if *theirs == ours
        );
        !(other_namespace && same_boot)
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("markers always serialize")
    }

    /// Whether the process is still that Terraform run, not one that reused
    /// its pid.
    pub fn is_running(&self) -> bool {
        start_ticks(self.pid) == Some(self.start_ticks)
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn pid_namespace() -> Option<String> {
    std::fs::read_link("/proc/self/ns/pid")
        .ok()
        .map(|link| link.to_string_lossy().into_owned())
}

fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_string())
}

/// Field 22 of `/proc/<pid>/stat`.
fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2, the command, is in parentheses and may hold spaces or ')'.
    let after_command = &stat[stat.rfind(')')? + 1..];
    after_command.split_whitespace().nth(19)?.parse().ok()
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The marker of the last run in `dir`, if it didn't clean up after itself
/// and this app can tell whether that run is alive. A run it can't see
/// counts as no evidence either way; its state lock speaks for it.
pub async fn run_marker(dir: &Path) -> Option<RunMarker> {
    let json = tokio::fs::read(dir.join(RUN_MARKER)).await.ok()?;
    serde_json::from_slice::<RunMarker>(&json)
        .ok()
        .filter(RunMarker::is_visible)
}

/// Waits while an earlier Terraform run in `dir` is still going, such as one
/// that kept running after the app was closed (outside Flatpak; see
/// [`run_marker`] for runs in another sandbox). Returns its marker when that
/// run is gone without cleaning up, which means it was killed and may have
/// left its state lock behind.
pub async fn wait_for_earlier_run(
    dir: &Path,
    at_most: Duration,
) -> ProviderResult<Option<RunMarker>> {
    let Some(marker) = run_marker(dir).await else {
        return Ok(None);
    };
    let deadline = Instant::now() + at_most;
    while marker.is_running() {
        if Instant::now() >= deadline {
            return Err(ProviderError::InvalidState(
                "an earlier change to this machine is still running; try again when it's done"
                    .into(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(run_marker(dir).await.filter(|m| *m == marker))
}

/// What Terraform writes into a state's `.tflock` object while it holds it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LockInfo {
    #[serde(rename = "ID")]
    pub id: String,
    /// `user@host` of whoever took it.
    #[serde(rename = "Who", default)]
    pub who: String,
    /// RFC 3339, UTC.
    #[serde(rename = "Created", default)]
    pub created: String,
}

impl LockInfo {
    pub fn parse(json: &[u8]) -> Option<Self> {
        serde_json::from_slice(json).ok()
    }

    /// The host part of [`LockInfo::who`].
    pub fn host(&self) -> Option<&str> {
        self.who.rsplit_once('@').map(|(_, host)| host)
    }
}

/// This computer's host name, as Terraform puts it in its locks.
pub fn host_name() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
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

    /// A stand-in `terraform` that runs `script` with sh.
    fn script(dir: &Path, script: &str) -> TerraformCli {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("terraform");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        TerraformCli {
            binary: Some(path),
            plugin_cache: dir.join("plugins"),
        }
    }

    #[tokio::test]
    async fn terraform_sees_only_the_credentials_and_a_safe_environment() {
        let dir = tempfile::tempdir().unwrap();
        let tf = script(dir.path(), "env");
        let env = tf
            .run(
                dir.path(),
                &["version".into()],
                &[("AWS_ACCESS_KEY_ID".into(), "AKIATEST".into())],
            )
            .await
            .unwrap();
        assert!(env.contains("AWS_ACCESS_KEY_ID=AKIATEST"), "{env}");
        assert!(env.contains("TF_IN_AUTOMATION=1"));
        assert!(env.lines().any(|l| l.starts_with("PATH=")));
        // cargo sets these for the test run; Terraform mustn't see them.
        assert!(!env.contains("CARGO_"), "{env}");
        // Its output files and run marker are cleaned up.
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".terraform-") || n == RUN_MARKER)
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn failures_report_terraforms_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tf = script(
            dir.path(),
            "echo 'Error: Error acquiring the state lock' >&2; exit 1",
        );
        let err = tf
            .run(dir.path(), &["apply".into()], &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "terraform apply failed: Error acquiring the state lock"
        );
    }

    #[tokio::test]
    async fn a_run_marks_itself_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        // A launcher that execs the real binary, as snap and tfenv do.
        let tf = script(dir.path(), "exec sleep 1");
        let run = {
            let dir = dir.path().to_path_buf();
            tokio::spawn(async move { tf.run(&dir, &[], &[]).await })
        };
        let mut marker = None;
        for _ in 0..100 {
            marker = run_marker(dir.path()).await;
            if marker.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let marker = marker.expect("the run writes its marker");
        assert!(marker.is_running());
        assert!(marker.started > 0);
        assert_eq!(marker.pid_namespace, pid_namespace());
        assert_eq!(marker.boot_id, boot_id());
        run.await.unwrap().unwrap();
        assert!(!marker.is_running());
        assert_eq!(run_marker(dir.path()).await, None);
    }

    #[tokio::test]
    async fn waits_only_for_a_live_earlier_run() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Ok(None)
        );
        // This test process stands in for a run that is still going.
        let live = RunMarker::of(std::process::id()).unwrap();
        assert!(live.is_running());
        // Another process that got the same pid later.
        let reused = RunMarker {
            start_ticks: live.start_ticks + 1,
            ..live.clone()
        };
        assert!(!reused.is_running());
        std::fs::write(dir.path().join(RUN_MARKER), live.to_json()).unwrap();
        assert!(matches!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Err(ProviderError::InvalidState(_))
        ));
        // A killed run's marker comes back, for its lock to be cleared.
        let dead = RunMarker {
            pid: u32::MAX,
            ..live
        };
        assert!(!dead.is_running());
        std::fs::write(dir.path().join(RUN_MARKER), dead.to_json()).unwrap();
        assert_eq!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Ok(Some(dead))
        );
    }

    #[tokio::test]
    async fn a_run_in_another_sandbox_is_no_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let write = |marker: &RunMarker| {
            std::fs::write(dir.path().join(RUN_MARKER), marker.to_json()).unwrap();
        };
        // Started by an earlier Flatpak sandbox in this boot, which stays up
        // while its Terraform runs: the pid means nothing here, so the run is
        // neither waited for nor taken for dead.
        let elsewhere = RunMarker {
            pid: u32::MAX,
            start_ticks: 1,
            started: unix_now(),
            pid_namespace: Some("pid:[1]".into()),
            boot_id: boot_id(),
        };
        assert!(boot_id().is_some() && pid_namespace().is_some());
        write(&elsewhere);
        assert_eq!(run_marker(dir.path()).await, None);
        assert_eq!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Ok(None)
        );
        // From an earlier boot, it's dead whichever sandbox ran it.
        let before_reboot = RunMarker {
            boot_id: Some("an earlier boot".into()),
            ..elsewhere.clone()
        };
        write(&before_reboot);
        assert_eq!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Ok(Some(before_reboot))
        );
        // Markers written before these fields existed read as this app's.
        let older_app = RunMarker {
            pid_namespace: None,
            boot_id: None,
            ..elsewhere
        };
        write(&older_app);
        assert_eq!(
            wait_for_earlier_run(dir.path(), Duration::ZERO).await,
            Ok(Some(older_app))
        );
    }

    #[test]
    fn reads_terraforms_lock_info() {
        let lock = LockInfo::parse(
            br#"{"ID": "6a1c", "Operation": "OperationTypeApply", "Info": "",
                "Who": "sergi@my-pc", "Version": "1.16.5",
                "Created": "2026-10-11T03:21:12.123456789Z", "Path": "b/k"}"#,
        )
        .unwrap();
        assert_eq!(lock.id, "6a1c");
        assert_eq!(lock.host(), Some("my-pc"));
        assert_eq!(LockInfo::parse(b"not json"), None);
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

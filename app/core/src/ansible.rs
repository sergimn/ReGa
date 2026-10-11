//! Runs the Ansible playbook in `infra/ansible` against a machine: the setup
//! after every start (`site.yml`) and the agent actions the app needs
//! (`agent.yml`). The playbook is compiled into the app, like the Terraform
//! module, so the app always runs the playbook it was built and tested with.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::provider::{ProviderError, ProviderResult};
use crate::setup::{MachineAccess, MachineSetup};
use crate::terraform::{find_on_path, io_error};

/// `infra/ansible` without its tests and examples, file by file. A test
/// checks nothing is missing.
pub const PLAYBOOK: &[(&str, &str)] = &[
    (
        "agent.yml",
        include_str!("../../../infra/ansible/agent.yml"),
    ),
    (
        "ansible.cfg",
        include_str!("../../../infra/ansible/ansible.cfg"),
    ),
    (
        "group_vars/rega.yml",
        include_str!("../../../infra/ansible/group_vars/rega.yml"),
    ),
    (
        "roles/base/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/base/defaults/main.yml"),
    ),
    (
        "roles/base/handlers/main.yml",
        include_str!("../../../infra/ansible/roles/base/handlers/main.yml"),
    ),
    (
        "roles/base/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/base/tasks/main.yml"),
    ),
    (
        "roles/gpu/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/gpu/defaults/main.yml"),
    ),
    (
        "roles/gpu/files/rega-gpu-setup.ps1",
        include_str!("../../../infra/ansible/roles/gpu/files/rega-gpu-setup.ps1"),
    ),
    (
        "roles/gpu/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/gpu/tasks/main.yml"),
    ),
    (
        "roles/gpu/tasks/setup_now.yml",
        include_str!("../../../infra/ansible/roles/gpu/tasks/setup_now.yml"),
    ),
    (
        "roles/gpu/vars/main.yml",
        include_str!("../../../infra/ansible/roles/gpu/vars/main.yml"),
    ),
    (
        "roles/library_agent/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/library_agent/defaults/main.yml"),
    ),
    (
        "roles/library_agent/files/ReGa.Agent.psm1",
        include_str!("../../../infra/ansible/roles/library_agent/files/ReGa.Agent.psm1"),
    ),
    (
        "roles/library_agent/files/rega-agent.ps1",
        include_str!("../../../infra/ansible/roles/library_agent/files/rega-agent.ps1"),
    ),
    (
        "roles/library_agent/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/library_agent/tasks/main.yml"),
    ),
    (
        "roles/library_agent/vars/main.yml",
        include_str!("../../../infra/ansible/roles/library_agent/vars/main.yml"),
    ),
    (
        "roles/steam/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/steam/defaults/main.yml"),
    ),
    (
        "roles/steam/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/steam/tasks/main.yml"),
    ),
    (
        "roles/sunshine/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/sunshine/defaults/main.yml"),
    ),
    (
        "roles/sunshine/files/set-credentials.ps1",
        include_str!("../../../infra/ansible/roles/sunshine/files/set-credentials.ps1"),
    ),
    (
        "roles/sunshine/handlers/main.yml",
        include_str!("../../../infra/ansible/roles/sunshine/handlers/main.yml"),
    ),
    (
        "roles/sunshine/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/sunshine/tasks/main.yml"),
    ),
    (
        "roles/wireguard/defaults/main.yml",
        include_str!("../../../infra/ansible/roles/wireguard/defaults/main.yml"),
    ),
    (
        "roles/wireguard/files/configure-tunnel.ps1",
        include_str!("../../../infra/ansible/roles/wireguard/files/configure-tunnel.ps1"),
    ),
    (
        "roles/wireguard/tasks/main.yml",
        include_str!("../../../infra/ansible/roles/wireguard/tasks/main.yml"),
    ),
    (
        "roles/wireguard/templates/wireguard-client.conf.j2",
        include_str!("../../../infra/ansible/roles/wireguard/templates/wireguard-client.conf.j2"),
    ),
    ("site.yml", include_str!("../../../infra/ansible/site.yml")),
];

/// UDP port of the WireGuard server; the Terraform module's default.
pub const WIREGUARD_PORT: u16 = 51820;

/// Minutes without a stream, download or WinRM session before the machine
/// stops itself.
pub const IDLE_MINUTES: u32 = 15;

/// Environment variable the inventory reads the Administrator password from,
/// so it is never written to disk or put on a command line.
const PASSWORD_ENV: &str = "REGA_ADMIN_PASSWORD";

/// What `ansible-playbook` may see of the app's environment, besides what
/// the app sets. Anything else, such as `ANSIBLE_CONFIG` or
/// `ANSIBLE_INVENTORY`, could point a run somewhere else.
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
    "REQUESTS_CA_BUNDLE",
    // Where the Windows collections are, when not on Python's path.
    "ANSIBLE_COLLECTIONS_PATH",
    "ANSIBLE_HOME",
    "PYTHONPATH",
];

fn passes_through(name: &str) -> bool {
    PASSTHROUGH_ENV.contains(&name) || name.starts_with("LC_")
}

/// Runs `ansible-playbook`. Behind a trait so tests can record the calls.
#[async_trait]
pub trait Ansible: Send + Sync {
    /// Whether `ansible-playbook` is there to run.
    fn ready(&self) -> ProviderResult<()> {
        Ok(())
    }

    /// Runs `ansible-playbook <args>` in `dir` with `env` added. An error
    /// means it couldn't run at all; a failed playbook is a
    /// [`PlaybookOutput`] that says so.
    async fn run(
        &self,
        dir: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> ProviderResult<PlaybookOutput>;
}

/// What a run of `ansible-playbook` printed.
pub struct PlaybookOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl PlaybookOutput {
    /// Everything it printed, for a log.
    pub fn log(&self) -> String {
        if self.stderr.trim().is_empty() {
            return self.stdout.clone();
        }
        format!("{}\n--- stderr ---\n{}", self.stdout, self.stderr)
    }

    /// Its output, or which task failed and why.
    pub fn into_result(self) -> ProviderResult<String> {
        if self.success {
            return Ok(self.stdout);
        }
        Err(ProviderError::Cloud(summarize_failure(
            &self.stdout,
            &self.stderr,
        )))
    }
}

/// The real `ansible-playbook`.
pub struct AnsibleCli {
    binary: Option<PathBuf>,
}

impl AnsibleCli {
    /// Uses `REGA_ANSIBLE_PLAYBOOK` when set, otherwise `ansible-playbook`
    /// on `PATH`.
    pub fn find() -> Self {
        let binary = std::env::var_os("REGA_ANSIBLE_PLAYBOOK")
            .map(PathBuf::from)
            .or_else(|| find_on_path("ansible-playbook"));
        Self { binary }
    }

    pub fn at(binary: PathBuf) -> Self {
        Self {
            binary: Some(binary),
        }
    }

    fn binary(&self) -> ProviderResult<&PathBuf> {
        self.binary.as_ref().ok_or_else(|| {
            ProviderError::Cloud(
                "Ansible isn't installed: put ansible-playbook on PATH or set \
                 REGA_ANSIBLE_PLAYBOOK"
                    .into(),
            )
        })
    }
}

#[async_trait]
impl Ansible for AnsibleCli {
    fn ready(&self) -> ProviderResult<()> {
        self.binary().map(|_| ())
    }

    async fn run(
        &self,
        dir: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> ProviderResult<PlaybookOutput> {
        let binary = self.binary()?;
        let output = tokio::process::Command::new(binary)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(passes_through)))
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env("ANSIBLE_CONFIG", dir.join("ansible.cfg"))
            // Ansible's own temporary files, outside ~/.ansible, which a
            // sandbox may not let it write.
            .env("ANSIBLE_LOCAL_TEMP", dir.join(".tmp"))
            .env("ANSIBLE_NOCOLOR", "1")
            .env("ANSIBLE_FORCE_COLOR", "0")
            .env("ANSIBLE_RETRY_FILES_ENABLED", "0")
            .env("ANSIBLE_CALLBACK_RESULT_FORMAT", "json")
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| io_error("run Ansible", e))?;
        Ok(PlaybookOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// The task that failed and why, from `ansible-playbook`'s output: the
/// `fatal: [host]: FAILED! => {...}` line and the `TASK [...]` before it.
/// Falls back to Ansible's own `[ERROR]` lines, then to the last line.
pub fn summarize_failure(stdout: &str, stderr: &str) -> String {
    let mut task = None;
    let mut offset = 0;
    for line in stdout.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if let Some(name) = line
            .strip_prefix("TASK [")
            .and_then(|rest| rest.split_once("] ").map(|(name, _)| name))
        {
            // "role : task name"
            task = Some(name.rsplit(" : ").next().unwrap_or(name).to_string());
            continue;
        }
        if !line.starts_with("fatal: [") {
            continue;
        }
        let Some(arrow) = line.find("=> ") else {
            continue;
        };
        let json = &stdout[start + arrow + 3..];
        let result: Option<Value> = serde_json::Deserializer::from_str(json)
            .into_iter()
            .next()
            .and_then(Result::ok);
        let msg = result.as_ref().and_then(|r| r["msg"].as_str());
        // A command that failed says why on its own output, better than
        // Ansible's "non-zero return code".
        let output = result.as_ref().and_then(|r| {
            ["stderr", "stdout"]
                .iter()
                .filter_map(|k| r[*k].as_str())
                .map(str::trim)
                .find(|s| !s.is_empty())
                .map(String::from)
        });
        let msg = msg
            .filter(|m| !m.is_empty() && *m != "non-zero return code")
            .map(|m| m.trim_start_matches("Task failed: ").to_string())
            .or(output)
            .or_else(|| msg.filter(|m| !m.is_empty()).map(String::from))
            .unwrap_or_else(|| "failed".into());
        return match &task {
            Some(task) => format!("{task}: {msg}"),
            None => msg,
        };
    }
    for text in [stderr, stdout] {
        if let Some(error) = text.lines().find_map(|l| {
            l.strip_prefix("[ERROR]: ")
                .or_else(|| l.strip_prefix("ERROR! "))
        }) {
            return error.trim().to_string();
        }
    }
    stderr
        .lines()
        .chain(stdout.lines())
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("Ansible failed")
        .trim()
        .to_string()
}

/// Writes the playbook into `dir`, replacing what an older app version put
/// there.
pub async fn prepare_playbook(dir: &Path) -> ProviderResult<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(io_error("clear the old playbook", e));
        }
        _ => {}
    }
    for (name, contents) in PLAYBOOK {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_error("create the playbook folder", e))?;
        }
        tokio::fs::write(&path, contents)
            .await
            .map_err(|e| io_error("write the playbook", e))?;
    }
    Ok(())
}

/// The inventory for one machine: how to reach it and the variables the
/// playbook takes from the app. JSON, which Ansible reads as YAML.
pub fn inventory(machine: &MachineAccess, secrets_dir: &Path) -> ProviderResult<String> {
    let host = machine.public_ip.as_deref().ok_or_else(|| {
        ProviderError::InvalidState("the machine has no public IP; is it running?".into())
    })?;
    let vars = json!({
        "ansible_host": host,
        "ansible_password": format!("{{{{ lookup('ansible.builtin.env', '{PASSWORD_ENV}') }}}}"),
        "rega_machine_id": machine.machine_id,
        "rega_secrets_dir": secrets_dir,
        "rega_library_bucket": machine.library_bucket,
        "rega_library_bucket_region": machine.library_region,
        "rega_wireguard_port": WIREGUARD_PORT,
        "rega_idle_minutes": IDLE_MINUTES,
    });
    let inventory = json!({ "rega": { "hosts": { machine.machine_id.clone(): vars } } });
    Ok(serde_json::to_string_pretty(&inventory).expect("inventories always serialize"))
}

/// Writes a private file, creating its folder (also private).
async fn write_private(path: &Path, contents: &str) -> ProviderResult<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = path.to_path_buf();
    let contents = contents.to_string();
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?
            .write_all(contents.as_bytes())
    })
    .await
    .map_err(|e| ProviderError::Cloud(format!("couldn't save a machine file: {e}")))?
    .map_err(|e| io_error("save a machine file", e))
}

/// Sets machines up with the playbook, over WinRM.
pub struct AnsibleSetup {
    ansible: Arc<dyn Ansible>,
}

impl AnsibleSetup {
    pub fn new(ansible: Arc<dyn Ansible>) -> Self {
        Self { ansible }
    }

    /// Uses the `ansible-playbook` [`AnsibleCli::find`] finds.
    pub fn find() -> Self {
        Self::new(Arc::new(AnsibleCli::find()))
    }

    /// Runs `playbook` against the machine, keeping its output in
    /// `<machine folder>/<log>` for troubleshooting.
    async fn run(
        &self,
        machine: &MachineAccess,
        playbook: &str,
        extra: &[&str],
        log: &str,
    ) -> ProviderResult<String> {
        let password = machine.admin_password.clone().ok_or_else(|| {
            ProviderError::InvalidState("the machine hasn't made its Windows password yet".into())
        })?;
        let dir = machine.dir.join("ansible");
        let secrets = machine.dir.join("secrets");
        prepare_playbook(&dir).await?;
        // The playbook uses these rather than making its own, so every
        // device sets the same ones.
        write_private(
            &secrets.join("sunshine-password"),
            &machine.secrets.sunshine_password,
        )
        .await?;
        write_private(
            &secrets.join("desktop-password"),
            &machine.secrets.desktop_password,
        )
        .await?;
        write_private(&dir.join("inventory.json"), &inventory(machine, &secrets)?).await?;
        let mut args: Vec<String> = vec!["-i".into(), "inventory.json".into(), playbook.into()];
        for var in extra {
            args.extend(["-e".to_string(), var.to_string()]);
        }
        let result = self
            .ansible
            .run(&dir, &args, &[(PASSWORD_ENV.into(), password)])
            .await;
        let text = match &result {
            Ok(output) => output.log(),
            Err(e) => e.to_string(),
        };
        // Best effort: the log only helps troubleshooting.
        let _ = write_private(&machine.dir.join(log), &text).await;
        result?.into_result()
    }
}

#[async_trait]
impl MachineSetup for AnsibleSetup {
    fn ready(&self) -> ProviderResult<()> {
        self.ansible.ready()
    }

    async fn after_start(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.run(machine, "site.yml", &[], "setup.log")
            .await
            .map(|_| ())
            .map_err(|e| ProviderError::Cloud(format!("setting up the machine failed: {e}")))
    }

    async fn wait_for_library(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.run(
            machine,
            "agent.yml",
            &["rega_agent_action=WaitRestore"],
            "restore.log",
        )
        .await
        .map(|_| ())
        .map_err(|e| ProviderError::Cloud(format!("restoring the game library failed: {e}")))
    }

    async fn before_stop(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.run(
            machine,
            "agent.yml",
            &["rega_agent_action=FinalSync"],
            "final-sync.log",
        )
        .await
        .map(|_| ())
        .map_err(|e| ProviderError::Cloud(format!("uploading the game library failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::MachineSecrets;
    use std::sync::Mutex;

    #[test]
    fn every_playbook_file_is_embedded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/ansible");
        fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = format!("{rel}{name}");
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), &format!("{path}/"), out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut on_disk = Vec::new();
        walk(&root, "", &mut on_disk);
        // What the app doesn't run: docs, examples, the controller's own
        // requirements, lint settings and tests.
        let mut on_disk: Vec<String> = on_disk
            .into_iter()
            .filter(|p| {
                !p.starts_with('.')
                    && !p.starts_with("tests/")
                    && !p.starts_with("requirements.")
                    && p != "README.md"
                    && p != "inventory.example.yml"
            })
            .collect();
        on_disk.sort();
        let embedded: Vec<String> = PLAYBOOK.iter().map(|(n, _)| n.to_string()).collect();
        assert_eq!(embedded, on_disk);
    }

    const FAILED: &str = r#"
PLAY [Set up a ReGa machine] ***************************************************

TASK [Wait for WinRM] **********************************************************
ok: [m]

TASK [sunshine : Install Sunshine] *********************************************
[ERROR]: Task failed: Module failed: download failed
Origin: /x/roles/sunshine/tasks/main.yml:11:3

fatal: [m]: FAILED! => {"changed": false, "msg": "Task failed: Module failed: download \"failed\""}

PLAY RECAP *********************************************************************
m                          : ok=1    changed=0    unreachable=0    failed=1
"#;

    #[test]
    fn failures_name_the_task_and_why() {
        assert_eq!(
            summarize_failure(FAILED, ""),
            r#"Install Sunshine: Module failed: download "failed""#
        );
        let unreachable = "TASK [Wait for WinRM] ***\nfatal: [m]: UNREACHABLE! => \
                           {\"changed\": false, \"msg\": \"ssl: connection reset\", \"unreachable\": true}\n";
        // Only FAILED and UNREACHABLE lines start with "fatal:".
        assert_eq!(
            summarize_failure(unreachable, ""),
            "Wait for WinRM: ssl: connection reset"
        );
        let command =
            "TASK [Run the agent] ***\nfatal: [m]: FAILED! => {\n    \"msg\": \"\",\n    \
                       \"rc\": 1,\n    \"stderr\": \"no bucket\\r\\n\",\n    \"stdout\": \"\"\n}\n";
        assert_eq!(summarize_failure(command, ""), "Run the agent: no bucket");
        let command = command.replace(r#""msg": """#, r#""msg": "non-zero return code""#);
        assert_eq!(summarize_failure(&command, ""), "Run the agent: no bucket");
        assert_eq!(
            summarize_failure("", "[ERROR]: the playbook: x.yml could not be found\n"),
            "the playbook: x.yml could not be found"
        );
        assert_eq!(
            summarize_failure("", "Traceback\nImportError: winrm\n"),
            "ImportError: winrm"
        );
        assert_eq!(summarize_failure("", ""), "Ansible failed");
    }

    fn machine(dir: &Path) -> MachineAccess {
        MachineAccess {
            machine_id: "gaming-abc123".into(),
            region_id: "eu-central-1".into(),
            instance_id: "i-1".into(),
            type_name: "t3.medium".into(),
            public_ip: Some("198.51.100.1".into()),
            admin_password: Some("Pa55word!".into()),
            wireguard: true,
            dir: dir.to_path_buf(),
            library_bucket: "rega-games-123456789012".into(),
            library_region: "eu-north-1".into(),
            secrets: MachineSecrets {
                sunshine_password: "sun".into(),
                desktop_password: "desk-Rg7".into(),
            },
        }
    }

    #[test]
    fn the_inventory_never_holds_the_password() {
        let dir = Path::new("/data/m");
        let text = inventory(&machine(dir), &dir.join("secrets")).unwrap();
        assert!(!text.contains("Pa55word!"));
        let json: Value = serde_json::from_str(&text).unwrap();
        let host = &json["rega"]["hosts"]["gaming-abc123"];
        assert_eq!(host["ansible_host"], "198.51.100.1");
        assert_eq!(
            host["ansible_password"],
            "{{ lookup('ansible.builtin.env', 'REGA_ADMIN_PASSWORD') }}"
        );
        assert_eq!(host["rega_secrets_dir"], "/data/m/secrets");
        assert_eq!(host["rega_library_bucket"], "rega-games-123456789012");
        assert_eq!(host["rega_library_bucket_region"], "eu-north-1");
        assert_eq!(host["rega_idle_minutes"], 15);

        let mut stopped = machine(dir);
        stopped.public_ip = None;
        assert!(inventory(&stopped, dir).is_err());
    }

    /// One `ansible-playbook` run: its folder, arguments and environment.
    type Run = (PathBuf, Vec<String>, Vec<(String, String)>);

    #[derive(Default)]
    struct Recorder {
        runs: Mutex<Vec<Run>>,
        fail: Option<String>,
    }

    #[async_trait]
    impl Ansible for Recorder {
        async fn run(
            &self,
            dir: &Path,
            args: &[String],
            env: &[(String, String)],
        ) -> ProviderResult<PlaybookOutput> {
            assert!(dir.join("site.yml").exists());
            assert!(dir.join(&args[1]).exists(), "{args:?}");
            self.runs
                .lock()
                .unwrap()
                .push((dir.to_path_buf(), args.to_vec(), env.to_vec()));
            Ok(PlaybookOutput {
                success: self.fail.is_none(),
                stdout: self.fail.clone().unwrap_or_else(|| "ok".into()),
                stderr: "a warning".into(),
            })
        }
    }

    #[tokio::test]
    async fn setup_runs_the_site_playbook_with_the_machines_passwords() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let setup = AnsibleSetup::new(recorder.clone());
        let m = machine(tmp.path());
        setup.after_start(&m).await.unwrap();
        setup.wait_for_library(&m).await.unwrap();
        setup.before_stop(&m).await.unwrap();

        let runs = recorder.runs.lock().unwrap();
        let args: Vec<Vec<String>> = runs.iter().map(|r| r.1.clone()).collect();
        assert_eq!(args[0], ["-i", "inventory.json", "site.yml"]);
        assert_eq!(
            args[1],
            [
                "-i",
                "inventory.json",
                "agent.yml",
                "-e",
                "rega_agent_action=WaitRestore"
            ]
        );
        assert_eq!(
            args[2],
            [
                "-i",
                "inventory.json",
                "agent.yml",
                "-e",
                "rega_agent_action=FinalSync"
            ]
        );
        assert_eq!(runs[0].0, tmp.path().join("ansible"));
        assert_eq!(
            runs[0].2,
            [("REGA_ADMIN_PASSWORD".to_string(), "Pa55word!".to_string())]
        );

        let secrets = tmp.path().join("secrets");
        assert_eq!(
            std::fs::read_to_string(secrets.join("sunshine-password")).unwrap(),
            "sun"
        );
        assert_eq!(
            std::fs::read_to_string(secrets.join("desktop-password")).unwrap(),
            "desk-Rg7"
        );
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(secrets.clone()), 0o700);
        assert_eq!(mode(secrets.join("sunshine-password")), 0o600);
        assert!(tmp.path().join("setup.log").exists());
        assert!(tmp.path().join("final-sync.log").exists());
    }

    #[tokio::test]
    async fn setup_needs_the_windows_password() {
        let tmp = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let setup = AnsibleSetup::new(recorder.clone());
        let mut m = machine(tmp.path());
        m.admin_password = None;
        assert!(setup.after_start(&m).await.is_err());
        assert!(recorder.runs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_setup_says_where_and_keeps_the_log() {
        let tmp = tempfile::tempdir().unwrap();
        let setup = AnsibleSetup::new(Arc::new(Recorder {
            fail: Some(FAILED.into()),
            ..Recorder::default()
        }));
        let err = setup.after_start(&machine(tmp.path())).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"setting up the machine failed: Install Sunshine: Module failed: download "failed""#
        );
        // All of it, for troubleshooting.
        let log = std::fs::read_to_string(tmp.path().join("setup.log")).unwrap();
        assert!(log.starts_with(FAILED), "{log}");
        assert!(log.ends_with("--- stderr ---\na warning"), "{log}");
    }

    /// A stand-in `ansible-playbook` that runs `script` with sh.
    fn script(dir: &Path, script: &str) -> AnsibleCli {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ansible-playbook");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        AnsibleCli::at(path)
    }

    #[tokio::test]
    async fn ansible_sees_only_a_safe_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = script(tmp.path(), "env");
        let env = cli
            .run(
                tmp.path(),
                &[],
                &[("REGA_ADMIN_PASSWORD".into(), "x".into())],
            )
            .await
            .unwrap()
            .into_result()
            .unwrap();
        assert!(env.contains("REGA_ADMIN_PASSWORD=x"), "{env}");
        assert!(env.contains(&format!(
            "ANSIBLE_CONFIG={}",
            tmp.path().join("ansible.cfg").display()
        )));
        assert!(env.contains("ANSIBLE_NOCOLOR=1"));
        assert!(env.lines().any(|l| l.starts_with("PATH=")));
        assert!(!env.contains("CARGO_"), "{env}");
    }

    #[tokio::test]
    async fn ansible_failures_are_summarized() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("out"), FAILED).unwrap();
        let cli = script(tmp.path(), "cat out; echo oops >&2; exit 2");
        let output = cli.run(tmp.path(), &[], &[]).await.unwrap();
        assert!(!output.success);
        assert_eq!(output.stderr, "oops\n");
        let err = output.into_result().unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"Install Sunshine: Module failed: download "failed""#
        );
        assert!(cli.ready().is_ok());
        let missing = AnsibleCli { binary: None };
        let Err(err) = missing.run(tmp.path(), &[], &[]).await else {
            panic!("ran without ansible-playbook");
        };
        assert!(err.to_string().contains("REGA_ANSIBLE_PLAYBOOK"));
        // Checked before a machine comes up, too.
        let setup = AnsibleSetup::new(Arc::new(missing));
        assert!(setup
            .ready()
            .unwrap_err()
            .to_string()
            .contains("isn't installed"));
    }

    /// With the real `ansible-playbook` (CI installs it): the embedded
    /// playbooks parse, and the inventory the app writes names the machine.
    #[tokio::test]
    async fn the_real_ansible_reads_the_playbook_and_inventory() {
        let cli = AnsibleCli::find();
        if cli.binary.is_none() {
            assert!(
                std::env::var_os("REGA_REQUIRE_ANSIBLE").is_none(),
                "ansible-playbook isn't installed"
            );
            eprintln!("skipped: ansible-playbook isn't installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ansible");
        prepare_playbook(&dir).await.unwrap();
        let m = machine(tmp.path());
        std::fs::write(
            dir.join("inventory.json"),
            inventory(&m, &tmp.path().join("secrets")).unwrap(),
        )
        .unwrap();
        let env = [("REGA_ADMIN_PASSWORD".to_string(), "x".to_string())];
        for playbook in ["site.yml", "agent.yml"] {
            let args = ["-i", "inventory.json", playbook, "--syntax-check"].map(String::from);
            cli.run(&dir, &args, &env)
                .await
                .unwrap()
                .into_result()
                .unwrap();
        }
        let args = ["-i", "inventory.json", "site.yml", "--list-hosts"].map(String::from);
        let hosts = cli
            .run(&dir, &args, &env)
            .await
            .unwrap()
            .into_result()
            .unwrap();
        assert!(hosts.contains("gaming-abc123"), "{hosts}");
    }
}

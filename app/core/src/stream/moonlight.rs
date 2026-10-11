//! Finds Moonlight on this computer and runs its command line (moonlight-qt's
//! `app/cli/commandlineparser.cpp`): `pair <host> --pin <pin>`,
//! `stream <host> <app>` and `list <host>`, which fails until the host is
//! paired.
//!
//! Inside ReGa's Flatpak there is no Moonlight to run: the sandbox can't
//! start host programs, so pairing there is by the PIN Moonlight shows and
//! the user streams from Moonlight's own window.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

/// Moonlight's id on Flathub.
pub const FLATHUB_ID: &str = "com.moonlight_stream.Moonlight";
/// How long `list` may take to reach the host.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// A Moonlight this computer can run: a program and the arguments that come
/// before Moonlight's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moonlight {
    program: PathBuf,
    prefix: Vec<OsString>,
}

/// Whether ReGa itself runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

impl Moonlight {
    /// `REGA_MOONLIGHT`, then `moonlight` or `moonlight-qt` on `PATH`, then
    /// the Flathub app when it is installed. `None` inside ReGa's Flatpak,
    /// unless `REGA_MOONLIGHT` names one.
    pub async fn find() -> Option<Self> {
        let flatpak = if in_flatpak() {
            None
        } else {
            find_on_path(std::env::var_os("PATH").as_deref(), "flatpak")
        };
        Self::find_with(
            std::env::var_os("REGA_MOONLIGHT"),
            std::env::var_os("PATH"),
            flatpak,
        )
        .await
    }

    /// [`Moonlight::find`] with its inputs spelled out: `flatpak` is the
    /// `flatpak` program to look for the Flathub app with, if any.
    pub async fn find_with(
        override_path: Option<OsString>,
        path: Option<OsString>,
        flatpak: Option<PathBuf>,
    ) -> Option<Self> {
        if let Some(program) = override_path.filter(|p| !p.is_empty()) {
            return Some(Self::program(program.into()));
        }
        for name in ["moonlight", "moonlight-qt"] {
            if let Some(program) = find_on_path(path.as_deref(), name) {
                return Some(Self::program(program));
            }
        }
        let flatpak = flatpak?;
        let installed = Command::new(&flatpak)
            .args(["info", FLATHUB_ID])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        match tokio::time::timeout(Duration::from_secs(10), installed).await {
            Ok(Ok(status)) if status.success() => Some(Self {
                program: flatpak,
                prefix: vec!["run".into(), FLATHUB_ID.into()],
            }),
            _ => None,
        }
    }

    fn program(program: PathBuf) -> Self {
        Self {
            program,
            prefix: Vec::new(),
        }
    }

    /// Starts pairing with `host` using `pin`; it exits 0 once Sunshine
    /// has accepted the PIN. Killed if dropped before then.
    pub fn pair(&self, host: &str, pin: &str) -> std::io::Result<Child> {
        self.command(&["pair", host, "--pin", pin])
            .kill_on_drop(true)
            .spawn()
    }

    /// Whether Moonlight has `host` paired: `list` only works then. Also
    /// false when it can't reach the host at all.
    pub async fn is_paired(&self, host: &str) -> bool {
        let list = self.command(&["list", host]).kill_on_drop(true).status();
        matches!(
            tokio::time::timeout(LIST_TIMEOUT, list).await,
            Ok(Ok(status)) if status.success()
        )
    }

    /// Starts streaming `app` from `host` in Moonlight's window. The stream
    /// carries on if ReGa stops waiting for it.
    pub fn stream(&self, host: &str, app: &str) -> std::io::Result<Child> {
        self.command(&["stream", host, app]).spawn()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl std::fmt::Display for Moonlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.program.display())?;
        for arg in &self.prefix {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

fn find_on_path(path: Option<&std::ffi::OsStr>, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An executable shell script in `dir`.
    pub(crate) fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn found_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let path = std::env::join_paths([&bin, &other]).unwrap();

        // Nothing at all.
        assert_eq!(
            Moonlight::find_with(None, Some(path.clone()), None).await,
            None
        );

        // A Flathub install, found through `flatpak info`.
        let log = dir.path().join("flatpak.log");
        let flatpak = script(
            dir.path(),
            "flatpak",
            &format!(
                "echo \"$@\" >> {}\n[ \"$2\" = {FLATHUB_ID} ]",
                log.display()
            ),
        );
        let found = Moonlight::find_with(None, Some(path.clone()), Some(flatpak.clone()))
            .await
            .unwrap();
        assert_eq!(
            found.to_string(),
            format!("{} run {FLATHUB_ID}", flatpak.display())
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("info {FLATHUB_ID}\n")
        );
        let not_installed = script(dir.path(), "flatpak-without", "exit 1");
        assert_eq!(
            Moonlight::find_with(None, Some(path.clone()), Some(not_installed)).await,
            None
        );

        // moonlight-qt on PATH beats the Flathub app; moonlight beats both.
        let qt = script(&other, "moonlight-qt", "exit 0");
        let found = Moonlight::find_with(None, Some(path.clone()), Some(flatpak.clone())).await;
        assert_eq!(found, Some(Moonlight::program(qt)));
        // Not executable: skipped.
        std::fs::write(bin.join("moonlight"), "").unwrap();
        let found = Moonlight::find_with(None, Some(path.clone()), None)
            .await
            .unwrap();
        assert!(found.to_string().ends_with("moonlight-qt"));
        let plain = script(&bin, "moonlight", "exit 0");
        let found = Moonlight::find_with(None, Some(path.clone()), None).await;
        assert_eq!(found, Some(Moonlight::program(plain)));

        // REGA_MOONLIGHT beats everything, even a path that isn't there yet.
        let chosen = Moonlight::find_with(Some("/opt/m/moonlight".into()), Some(path), None).await;
        assert_eq!(chosen, Some(Moonlight::program("/opt/m/moonlight".into())));
    }

    #[tokio::test]
    async fn pair_list_and_stream_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args.log");
        // Paired only once a pair has run.
        let fake = script(
            dir.path(),
            "moonlight",
            &format!(
                "echo \"$@\" >> {log}\ncase $1 in\n  pair) touch {dir}/paired ;;\n  list) [ -e {dir}/paired ] ;;\nesac",
                log = log.display(),
                dir = dir.path().display()
            ),
        );
        let moonlight = Moonlight::program(fake);
        assert!(!moonlight.is_paired("127.0.0.1").await);
        let status = moonlight
            .pair("127.0.0.1", "0420")
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(status.success());
        assert!(moonlight.is_paired("127.0.0.1").await);
        moonlight
            .stream("127.0.0.1", "Desktop")
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "list 127.0.0.1\npair 127.0.0.1 --pin 0420\nlist 127.0.0.1\nstream 127.0.0.1 Desktop\n"
        );
    }

    #[tokio::test]
    async fn the_flathub_app_gets_its_run_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args.log");
        let flatpak = script(
            dir.path(),
            "flatpak",
            &format!("echo \"$@\" >> {}", log.display()),
        );
        let moonlight = Moonlight::find_with(None, None, Some(flatpak))
            .await
            .unwrap();
        moonlight
            .pair("1.2.3.4", "1234")
            .unwrap()
            .wait()
            .await
            .unwrap();
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(
            logged.ends_with(&format!("run {FLATHUB_ID} pair 1.2.3.4 --pin 1234\n")),
            "{logged}"
        );
    }

    #[tokio::test]
    async fn a_missing_program_is_an_error_not_a_panic() {
        let moonlight = Moonlight::program("/nonexistent/moonlight".into());
        assert!(moonlight.pair("h", "1234").is_err());
        assert!(!moonlight.is_paired("h").await);
        assert!(moonlight.stream("h", "Desktop").is_err());
    }
}

//! Runs the user data stub in a sandbox, as the current user.
//!
//! A fake `curl` serves a prepared payload, a fake `sudo` drops `-u <user>`, and the fake
//! `chown` and `chmod` log their calls. The paths `/run` and `/var/log` move into a
//! temporary directory. The test needs `bash` and `jq`, and skips itself without `jq`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use fabrium_control_plane::bootstrap::stub;
use serde_json::{Value, json};

struct Sandbox {
    root: PathBuf,
}

/// What a run of the stub left behind.
struct Run {
    exit_ok: bool,
    /// The calls of the fake `chown` and `chmod`, in order.
    calls: Vec<String>,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fabrium-stub-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "run", "log", "home"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let sandbox = Self { root };
        // `curl` ignores its arguments but `-o`, reads the config from stdin, and serves the payload.
        sandbox.tool(
            "curl",
            "out=\"\"\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; *) shift;; esac; done\n\
             while IFS= read -r _; do :; done\ncp \"$FAKE_PAYLOAD\" \"$out\"\n",
        );
        sandbox.tool("sudo", "[ \"$1\" = \"-u\" ] && shift 2\nexec \"$@\"\n");
        sandbox.tool("chown", "echo \"chown $*\" >>\"$CALL_LOG\"\n");
        sandbox.tool(
            "chmod",
            "echo \"chmod $*\" >>\"$CALL_LOG\"\nexec /bin/chmod \"$@\"\n",
        );
        sandbox
    }

    fn tool(&self, name: &str, body: &str) {
        let path = self.root.join("bin").join(name);
        fs::write(&path, format!("#!/bin/bash\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Runs the stub with this payload.
    fn run(&self, payload: &Value) -> Run {
        let root = self.root.display().to_string();
        let script = stub("https://cp.example.org", "TESTTOKEN")
            .replace(
                "/var/log/fabrium-bootstrap.log",
                &format!("{root}/log/fabrium-bootstrap.log"),
            )
            .replace("/run/", &format!("{root}/run/"));
        fs::write(self.path("stub.sh"), script).unwrap();
        fs::write(self.path("payload.json"), payload.to_string()).unwrap();
        fs::write(self.path("calls.log"), "").unwrap();
        let path = format!("{root}/bin:{}", std::env::var("PATH").unwrap_or_default());
        let status = Command::new("bash")
            .arg(self.path("stub.sh"))
            .env("PATH", path)
            .env("FAKE_PAYLOAD", self.path("payload.json"))
            .env("CALL_LOG", self.path("calls.log"))
            .status()
            .unwrap();
        let calls = fs::read_to_string(self.path("calls.log")).unwrap();
        Run {
            exit_ok: status.success(),
            calls: calls.lines().map(str::to_owned).collect(),
        }
    }

    /// The names of the files that are left in the sandbox `/run`.
    fn left_in_run(&self) -> Vec<String> {
        let entries = fs::read_dir(self.path("run")).unwrap();
        entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The stub needs `jq`, like the VM image. Without it, the test has nothing to run.
fn have_jq() -> bool {
    let found = Command::new("jq").arg("--version").output().is_ok();
    if !found {
        eprintln!("skipped: jq is not installed");
    }
    found
}

fn payload(sandbox: &Sandbox, mode: &str, script: &str) -> Value {
    let env = sandbox.path("home/ubuntu/zooid/.env");
    let file = json!({ "path": env, "mode": mode, "owner": "ubuntu", "content": "SECRET=1\n" });
    json!({ "files": [file], "script": script })
}

fn is_in(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

#[test]
fn a_failed_setup_script_leaves_no_payload_in_run() {
    if !have_jq() {
        return;
    }
    let sandbox = Sandbox::new("failed");
    let run = sandbox.run(&payload(&sandbox, "600", "#!/bin/bash\nexit 1\n"));
    assert!(
        !run.exit_ok,
        "the stub must fail when the setup script fails"
    );
    // The payload holds secrets, and so does the setup script that came with it.
    assert_eq!(sandbox.left_in_run(), Vec::<String>::new());
    // The files that the payload names stay: they are the configuration of the VM.
    assert!(is_in(&sandbox.path("home/ubuntu/zooid"), ".env"));
}

#[test]
fn a_good_setup_script_leaves_no_payload_either() {
    if !have_jq() {
        return;
    }
    let sandbox = Sandbox::new("good");
    let run = sandbox.run(&payload(&sandbox, "600", "#!/bin/bash\ntrue\n"));
    assert!(run.exit_ok);
    assert_eq!(sandbox.left_in_run(), Vec::<String>::new());
}

#[test]
fn the_owner_is_set_before_the_mode() {
    if !have_jq() {
        return;
    }
    let sandbox = Sandbox::new("order");
    // chown clears the setuid and setgid bits, so the mode must come last.
    let run = sandbox.run(&payload(&sandbox, "4755", "#!/bin/bash\ntrue\n"));
    assert!(run.exit_ok);
    let env = sandbox.path("home/ubuntu/zooid/.env").display().to_string();
    let position = |tool: &str| {
        run.calls
            .iter()
            .position(|c| c.starts_with(tool) && c.ends_with(&env))
    };
    let (chown, chmod) = (position("chown ubuntu:"), position("chmod 4755"));
    assert!(chown.is_some() && chmod.is_some(), "{:?}", run.calls);
    assert!(
        chown < chmod,
        "chown must come before chmod: {:?}",
        run.calls
    );
}

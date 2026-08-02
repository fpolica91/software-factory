use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const SENTINEL_KEY: &str = "launcher-secret-must-not-print";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "factory-launcher-control-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_executable(path: &Path, contents: &[u8]) {
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn invoke(launcher: &Path, fake_bin: &Path, log: &Path, args: &[&str]) -> String {
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(launcher)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("PATH", path)
        .env("FACTORY_DOCKER_LOG", log)
        .env("OPENAI_API_KEY", SENTINEL_KEY)
        .output()
        .unwrap();
    let visible_output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!visible_output.contains(SENTINEL_KEY));
    let commands = std::fs::read_to_string(log).unwrap();
    assert!(!commands.contains(SENTINEL_KEY));
    commands
}

#[test]
fn completed_result_commands_start_only_the_control_plane() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let launcher = fixture.0.join("factory");
    std::fs::copy(repository_root.join("factory"), &launcher).unwrap();
    let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).unwrap();
    std::fs::write(
        fixture.0.join(".env"),
        b"FACTORY_IMAGE=software-factory:local\n",
    )
    .unwrap();
    std::fs::write(fixture.0.join(".env.example"), b"").unwrap();
    std::fs::write(fixture.0.join("docker-compose.yml"), b"services: {}\n").unwrap();

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf '%s\n' "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*) printf '%s\n' 'FACTORY_IMAGE=software-factory:local' ;;
  *" ps --status running --quiet factoryd "*) printf '%s\n' 'fake-factoryd' ;;
esac
exit 0
"##,
    );

    for args in [
        &["apply", "completed-job"][..],
        &["export", "completed-job", "-o", "-"][..],
    ] {
        let log = fixture.0.join(format!("{}.log", args[0]));
        let commands = invoke(&launcher, &fake_bin, &log, args);
        assert!(
            commands.contains("up -d --wait --wait-timeout 180 --remove-orphans postgres factoryd")
        );
        for forbidden in [
            "qdrant",
            "factory-worker",
            "claude-provider",
            "deepseek-provider",
            "zai-provider",
            "FACTORY_PROVIDER_ADAPTER",
            "FACTORY_MODEL",
        ] {
            assert!(
                !commands.contains(forbidden),
                "{args:?} unexpectedly used {forbidden}:\n{commands}"
            );
        }
    }
}

#[test]
fn configure_starts_only_the_control_plane_and_connects_to_factoryd() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let launcher = fixture.0.join("factory");
    std::fs::copy(repository_root.join("factory"), &launcher).unwrap();
    let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).unwrap();
    std::fs::write(
        fixture.0.join(".env"),
        b"FACTORY_IMAGE=software-factory:local\n",
    )
    .unwrap();
    std::fs::write(fixture.0.join(".env.example"), b"").unwrap();
    std::fs::write(fixture.0.join("docker-compose.yml"), b"services: {}\n").unwrap();

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf '%s\n' "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*) printf '%s\n' 'FACTORY_IMAGE=software-factory:local' ;;
esac
exit 0
"##,
    );

    let log = fixture.0.join("configure.log");
    let commands = invoke(
        &launcher,
        &fake_bin,
        &log,
        &[
            "configure",
            "--provider",
            "openai",
            "--model",
            "gpt-5.6-sol",
        ],
    );
    assert!(
        commands.contains("up -d --wait --wait-timeout 180 --remove-orphans postgres factoryd")
    );
    assert!(commands.contains("--network software-factory_default"));
    assert!(commands.contains("--env FACTORYD_URL=http://factoryd:8787"));
    for forbidden in [
        "qdrant",
        "factory-worker",
        "claude-provider",
        "deepseek-provider",
        "zai-provider",
        "OPENAI_API_KEY=",
        "ANTHROPIC_API_KEY=",
        "DEEPSEEK_API_KEY=",
        "ZAI_API_KEY=",
    ] {
        assert!(
            !commands.contains(forbidden),
            "configure unexpectedly used {forbidden}:\n{commands}"
        );
    }
}

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

const SENTINEL_KEY: &str = "launcher-secret-must-not-print";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "factory-launcher-control-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        // The launcher resolves physical paths (`pwd -P`); canonicalize so
        // path assertions agree on hosts where the temp dir is a symlink.
        Self(std::fs::canonicalize(&root).unwrap())
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

fn output_retrying_executable_file_busy(command: &mut Command) -> Output {
    const ATTEMPTS: usize = 8;

    for attempt in 1..=ATTEMPTS {
        match command.output() {
            Ok(output) => return output,
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < ATTEMPTS =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to execute copied launcher: {error}"),
        }
    }

    unreachable!("the bounded launcher retry loop always returns or panics")
}

fn invoke(launcher: &Path, fake_bin: &Path, log: &Path, args: &[&str]) -> String {
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = output_retrying_executable_file_busy(
        Command::new(launcher)
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("PATH", path)
            .env("FACTORY_DOCKER_LOG", log)
            .env("OPENAI_API_KEY", SENTINEL_KEY),
    );
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
        &["result", "completed-job"][..],
        &["artifacts", "completed-job"][..],
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

#[test]
fn launcher_creates_a_git_excluded_workspace_artifact_root() {
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
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&fixture.0)
            .status()
            .unwrap()
            .success()
    );

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf 'artifact=%s repository=%s command=%s\n' "$FACTORY_ARTIFACT_HOST_DIR" "$FACTORY_HOST_REPOSITORY_ID" "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*) printf '%s\n' 'FACTORY_IMAGE=software-factory:local' ;;
  *" ps --status running --quiet factoryd "*) printf '%s\n' 'fake-factoryd' ;;
esac
exit 0
"##,
    );

    let log = fixture.0.join("status.log");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = output_retrying_executable_file_busy(
        Command::new(&launcher)
            .args(["status", "completed-job"])
            .current_dir(&fixture.0)
            .env("PATH", path)
            .env("FACTORY_DOCKER_LOG", &log),
    );
    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(fixture.0.join(".factory/jobs").is_dir());
    let exclude = std::fs::read_to_string(fixture.0.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line == "/.factory/"));
    let status = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(&fixture.0)
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&status.stdout).contains(" .factory/"));

    let commands = std::fs::read_to_string(log).unwrap();
    assert!(commands.contains(&format!("artifact={}/.factory", fixture.0.display())));
    assert!(commands.contains("repository=local:"));
}

#[test]
fn kubernetes_image_reference_validation_precedes_launcher_mutation() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let launcher = fixture.0.join("factory");
    std::fs::copy(repository_root.join("factory"), &launcher).unwrap();
    let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).unwrap();
    std::fs::write(fixture.0.join(".env"), b"").unwrap();
    std::fs::write(fixture.0.join(".env.example"), b"").unwrap();
    std::fs::write(fixture.0.join("docker-compose.yml"), b"services: {}\n").unwrap();
    std::fs::write(
        fixture.0.join("docker-compose.kubernetes.yml"),
        b"services: {}\n",
    )
    .unwrap();

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf '%s\n' "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*)
    printf '%s\n' \
      'FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes'
    printf 'FACTORY_KUBERNETES_IMAGE=%s\n' "$FACTORY_TEST_KUBERNETES_IMAGE"
    printf 'FACTORY_KUBERNETES_KUBECONFIG=%s\n' "$FACTORY_TEST_KUBECONFIG"
    printf 'FACTORY_KUBERNETES_WORKSPACE_HOST_DIR=%s\n' "$FACTORY_TEST_WORKSPACE"
    ;;
esac
exit 0
"##,
    );
    write_executable(
        &fake_bin.join("kubectl"),
        br##"#!/bin/sh
printf 'kubectl %s\n' "$*" >> "$FACTORY_DOCKER_LOG"
exit 0
"##,
    );

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let digest = "a".repeat(64);
    let invalid_images = [
        "ghcr.io/example/software-factory:edge".to_string(),
        "factory@sha256:".to_string() + &digest,
        "ghcr.io/example/software-factory@sha256:".to_string(),
        format!("ghcr.io/example/software-factory@sha256:{}", "A".repeat(64)),
        format!("ghcr.io/example/software-factory@sha256:{}", "a".repeat(63)),
        format!("ghcr.io/example/software-factory@@sha256:{digest}"),
        format!("-ghcr.io/example/software-factory@sha256:{digest}"),
        format!("ghcr-.io/example/software-factory@sha256:{digest}"),
        format!("ghcr.io//example/software-factory@sha256:{digest}"),
        format!("ghcr.io/example//software-factory@sha256:{digest}"),
        format!("ghcr.io/example/software-factory/@sha256:{digest}"),
        format!("ghcr.io/example/software--factory@sha256:{digest}"),
        format!("ghcr.io/example/SoftwareFactory@sha256:{digest}"),
        format!("[::1]:5000/example/software-factory@sha256:{digest}"),
        format!("ghcr.io/example/software-factory:edge@sha256:{digest}"),
    ];
    let readable_kubeconfig = fixture.0.join("kubeconfig");
    std::fs::write(&readable_kubeconfig, b"apiVersion: v1\n").unwrap();
    let missing_kubeconfig = fixture.0.join("missing-kubeconfig");
    let workspace = fixture.0.join("workspaces");

    for (index, image) in invalid_images.iter().enumerate() {
        let log = fixture
            .0
            .join(format!("kubernetes-invalid-image-{index}.log"));
        let output = output_retrying_executable_file_busy(
            Command::new(&launcher)
                .arg("up")
                .current_dir(&fixture.0)
                .env("PATH", &path)
                .env("FACTORY_DOCKER_LOG", &log)
                .env("FACTORY_TEST_KUBERNETES_IMAGE", image)
                .env("FACTORY_TEST_KUBECONFIG", &readable_kubeconfig)
                .env("FACTORY_TEST_WORKSPACE", &workspace),
        );
        assert!(!output.status.success(), "invalid image accepted: {image}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("FACTORY_KUBERNETES_IMAGE must be an immutable registry digest"),
            "unexpected rejection for {image}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.0.join(".factory-execution-backend").exists());
        assert!(!fixture.0.join(".factory").exists());
        assert!(!workspace.exists());

        let commands = std::fs::read_to_string(log).unwrap();
        assert!(!commands.contains("kubectl"));
        assert!(!commands.contains(" volume "));
        assert!(!commands.contains(" up "));
    }

    let valid = format!("localhost:5000/example/software_factory.v2@sha256:{digest}");
    let log = fixture.0.join("kubernetes-valid-image.log");
    let output = output_retrying_executable_file_busy(
        Command::new(&launcher)
            .arg("up")
            .current_dir(&fixture.0)
            .env("PATH", path)
            .env("FACTORY_DOCKER_LOG", &log)
            .env("FACTORY_TEST_KUBERNETES_IMAGE", valid)
            .env("FACTORY_TEST_KUBECONFIG", &missing_kubeconfig)
            .env("FACTORY_TEST_WORKSPACE", &workspace),
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("K3s kubeconfig must be readable"),
        "valid digest did not reach kubeconfig preflight: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.0.join(".factory-execution-backend").exists());
    assert!(!fixture.0.join(".factory").exists());
    assert!(!workspace.exists());
    let commands = std::fs::read_to_string(log).unwrap();
    assert!(!commands.contains("kubectl"));
    assert!(!commands.contains(" up "));
}

#[test]
fn pinned_kubernetes_control_paths_do_not_require_an_execution_image() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let launcher = fixture.0.join("factory");
    std::fs::copy(repository_root.join("factory"), &launcher).unwrap();
    let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).unwrap();
    std::fs::write(fixture.0.join(".env"), b"").unwrap();
    std::fs::write(fixture.0.join(".env.example"), b"").unwrap();
    std::fs::write(fixture.0.join("docker-compose.yml"), b"services: {}\n").unwrap();
    std::fs::write(
        fixture.0.join("docker-compose.kubernetes.yml"),
        b"services: {}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join(".factory-execution-backend"),
        b"kubernetes\n",
    )
    .unwrap();

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf '%s\n' "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*)
    printf '%s\n' \
      'FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes' \
      'FACTORY_PROVIDER_ADAPTER=openai'
    printf 'FACTORY_KUBERNETES_IMAGE=%s\n' "$FACTORY_TEST_KUBERNETES_IMAGE"
    printf 'FACTORY_KUBERNETES_WORKSPACE_HOST_DIR=%s\n' "$FACTORY_TEST_WORKSPACE"
    ;;
esac
exit 0
"##,
    );

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let workspace = fixture.0.join("workspaces");
    for (image_name, image) in [
        ("empty", ""),
        ("malformed", "ghcr.io/example/software-factory:edge"),
    ] {
        for command in ["logs", "down"] {
            let log = fixture
                .0
                .join(format!("kubernetes-{image_name}-{command}.log"));
            let output = output_retrying_executable_file_busy(
                Command::new(&launcher)
                    .arg(command)
                    .current_dir(&fixture.0)
                    .env("PATH", &path)
                    .env("FACTORY_DOCKER_LOG", &log)
                    .env("FACTORY_TEST_KUBERNETES_IMAGE", image)
                    .env("FACTORY_TEST_WORKSPACE", &workspace),
            );
            assert!(
                output.status.success(),
                "{command} rejected {image_name} execution image: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            assert_eq!(
                std::fs::read_to_string(fixture.0.join(".factory-execution-backend")).unwrap(),
                "kubernetes\n"
            );
            assert!(!fixture.0.join(".factory").exists());
            assert!(!workspace.exists());

            let commands = std::fs::read_to_string(log).unwrap();
            assert!(commands.contains(&format!(
                "--file {}",
                fixture.0.join("docker-compose.kubernetes.yml").display()
            )));
            assert!(!commands.contains("kubectl"));
            assert!(!commands.contains(" up "));
            match command {
                "logs" => assert!(commands.contains("logs --follow factoryd factory-worker")),
                "down" => assert!(commands.contains("--profile * down --remove-orphans")),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn existing_pvc_workspace_mode_validates_operator_resources_without_mutating_them() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let launcher = fixture.0.join("factory");
    std::fs::copy(repository_root.join("factory"), &launcher).unwrap();
    let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).unwrap();
    std::fs::write(fixture.0.join(".env"), b"").unwrap();
    std::fs::write(fixture.0.join(".env.example"), b"").unwrap();
    std::fs::write(fixture.0.join("docker-compose.yml"), b"services: {}\n").unwrap();
    std::fs::write(
        fixture.0.join("docker-compose.kubernetes.yml"),
        b"services: {}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join(".factory-execution-backend"),
        b"kubernetes\n",
    )
    .unwrap();
    let kubeconfig = fixture.0.join("kubeconfig");
    std::fs::write(&kubeconfig, b"apiVersion: v1\n").unwrap();
    let workspace = fixture.0.join("shared-workspaces");
    std::fs::create_dir(&workspace).unwrap();
    let workspace_text = workspace.to_string_lossy().into_owned();

    write_executable(
        &fake_bin.join("docker"),
        br##"#!/bin/sh
printf 'ownership=%s docker %s\n' "${FACTORY_WORKSPACE_OWNERSHIP_MODE:-unset}" "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" config --environment "*)
    printf '%s\n' \
      'FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes' \
      'FACTORY_IMAGE=software-factory:local' \
      'FACTORY_PROVIDER_ADAPTER=openai' \
      'FACTORY_MODEL=gpt-5.6-sol' \
      'FACTORY_KUBERNETES_WORKSPACE_MODE=existing-pvc' \
      'FACTORY_KUBERNETES_WORKSPACE_PV=INVALID_VALUE' \
      'FACTORY_KUBERNETES_WORKSPACE_SIZE_GIB=not-a-size'
    printf 'OPENAI_API_KEY=%s\n' "$OPENAI_API_KEY"
    printf 'FACTORY_KUBERNETES_IMAGE=%s\n' "$FACTORY_TEST_KUBERNETES_IMAGE"
    printf 'FACTORY_KUBERNETES_KUBECONFIG=%s\n' "$FACTORY_TEST_KUBECONFIG"
    printf 'FACTORY_KUBERNETES_WORKSPACE_HOST_DIR=%s\n' "$FACTORY_TEST_WORKSPACE"
    printf 'FACTORY_KUBERNETES_NAMESPACE=%s\n' "$FACTORY_TEST_NAMESPACE"
    printf 'FACTORY_KUBERNETES_WORKSPACE_PVC=%s\n' "$FACTORY_TEST_PVC"
    ;;
esac
exit 0
"##,
    );
    write_executable(
        &fake_bin.join("kubectl"),
        br##"#!/bin/sh
printf 'kubectl %s\n' "$*" >> "$FACTORY_DOCKER_LOG"
case " $* " in
  *" get nodes -o "*) printf '%s\n' "$FACTORY_TEST_NODES" ;;
  *" get pvc "*) printf '%s' "$FACTORY_TEST_PVC_STATE" ;;
  *) exit 64 ;;
esac
"##,
    );
    write_executable(
        &fake_bin.join("chown"),
        br##"#!/bin/sh
printf 'chown %s\n' "$*" >> "$FACTORY_DOCKER_LOG"
exit 91
"##,
    );

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let digest = "a".repeat(64);
    let image = format!("ghcr.io/example/software-factory@sha256:{digest}");
    let ready_nodes = "ready-a||True\nready-b|false|True\ncordoned|true|True\nnot-ready||False";

    let run = |name: &str,
               workspace_value: &str,
               namespace: &str,
               pvc: &str,
               nodes: &str,
               pvc_state: &str| {
        let log = fixture.0.join(format!("existing-pvc-{name}.log"));
        let output = output_retrying_executable_file_busy(
            Command::new(&launcher)
                .arg("up")
                .current_dir(&fixture.0)
                .env("PATH", &path)
                .env("FACTORY_DOCKER_LOG", &log)
                .env("OPENAI_API_KEY", SENTINEL_KEY)
                .env("FACTORY_TEST_KUBERNETES_IMAGE", &image)
                .env("FACTORY_TEST_KUBECONFIG", &kubeconfig)
                .env("FACTORY_TEST_WORKSPACE", workspace_value)
                .env("FACTORY_TEST_NAMESPACE", namespace)
                .env("FACTORY_TEST_PVC", pvc)
                .env("FACTORY_TEST_NODES", nodes)
                .env("FACTORY_TEST_PVC_STATE", pvc_state),
        );
        (log, output)
    };

    for (name, workspace_value, namespace, pvc, expected) in [
        (
            "missing-workspace",
            "",
            "shared-factory",
            "shared-workspaces",
            "FACTORY_KUBERNETES_WORKSPACE_HOST_DIR is required",
        ),
        (
            "missing-namespace",
            workspace_text.as_str(),
            "",
            "shared-workspaces",
            "FACTORY_KUBERNETES_NAMESPACE is required",
        ),
        (
            "missing-pvc",
            workspace_text.as_str(),
            "shared-factory",
            "",
            "FACTORY_KUBERNETES_WORKSPACE_PVC is required",
        ),
    ] {
        let (log, output) = run(
            name,
            workspace_value,
            namespace,
            pvc,
            ready_nodes,
            "Bound||ReadWriteMany,",
        );
        assert!(!output.status.success(), "{name} unexpectedly passed");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{name} produced unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!std::fs::read_to_string(log).unwrap().contains("kubectl"));
    }

    for (name, nodes, pvc_state, expected) in [
        (
            "no-ready-nodes",
            "cordoned|true|True\nnot-ready||False",
            "Bound||ReadWriteMany,",
            "at least one Ready schedulable Kubernetes node",
        ),
        (
            "pending-claim",
            ready_nodes,
            "Pending||ReadWriteMany,",
            "must be Bound",
        ),
        (
            "block-claim",
            ready_nodes,
            "Bound|Block|ReadWriteMany,",
            "must use Filesystem volume mode",
        ),
        (
            "read-write-once-claim",
            ready_nodes,
            "Bound|Filesystem|ReadWriteOnce,",
            "must include ReadWriteMany access",
        ),
    ] {
        let (log, output) = run(
            name,
            &workspace_text,
            "shared-factory",
            "shared-workspaces",
            nodes,
            pvc_state,
        );
        assert!(!output.status.success(), "{name} unexpectedly passed");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{name} produced unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let commands = std::fs::read_to_string(log).unwrap();
        assert!(!commands.contains("apply -f"));
        assert!(!commands.contains("chown "));
        assert!(!commands.contains(" up "));
    }

    let (log, output) = run(
        "valid",
        &workspace_text,
        "shared-factory",
        "shared-workspaces",
        ready_nodes,
        "Bound||ReadWriteMany,ReadWriteOnce,",
    );
    assert!(
        output.status.success(),
        "valid existing PVC failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let visible_output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        visible_output
            .contains("existing PVC shared-factory/shared-workspaces, 2 Ready schedulable node(s)")
    );
    assert!(!visible_output.contains(SENTINEL_KEY));

    let commands = std::fs::read_to_string(log).unwrap();
    assert!(commands.contains("get pvc shared-workspaces"));
    assert!(commands.contains("ownership=preserve docker"));
    assert!(!commands.contains("apply -f"));
    assert!(!commands.contains("chown "));
    assert!(!commands.contains(SENTINEL_KEY));
}

#[test]
fn entrypoint_preserves_existing_pvc_workspace_ownership_and_compose_propagates_policy() {
    let fixture = TestRoot::new();
    let fake_bin = fixture.0.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let entrypoint = repository_root.join("apps/cli/factory-worker-entrypoint.sh");
    let compose_overlay =
        std::fs::read_to_string(repository_root.join("docker-compose.kubernetes.yml")).unwrap();
    let ownership_mapping =
        "FACTORY_WORKSPACE_OWNERSHIP_MODE: ${FACTORY_WORKSPACE_OWNERSHIP_MODE:-manage}";
    assert_eq!(
        compose_overlay.matches(ownership_mapping).count(),
        2,
        "the Kubernetes overlay must pass the derived policy to factoryd and factory-worker"
    );

    for command in ["mkdir", "chown", "cp", "chmod", "setpriv"] {
        write_executable(
            &fake_bin.join(command),
            format!(
                "#!/bin/sh\nprintf '{command} %s\\n' \"$*\" >> \"$FACTORY_ENTRYPOINT_LOG\"\nexit 0\n"
            )
            .as_bytes(),
        );
    }
    write_executable(
        &fake_bin.join("id"),
        b"#!/bin/sh\n[ \"${1:-}\" = -u ] && printf '0\\n'\n",
    );
    write_executable(&fake_bin.join("stat"), b"#!/bin/sh\nprintf '999\\n'\n");

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |name: &str, ownership_mode: Option<&str>| {
        let log = fixture.0.join(format!("entrypoint-{name}.log"));
        let mut command = Command::new("/bin/sh");
        command
            .arg(&entrypoint)
            .arg("/bin/true")
            .env("PATH", &path)
            .env("FACTORY_ENTRYPOINT_LOG", &log)
            .env("FACTORY_RUN_AS_UID", "1000")
            .env("FACTORY_RUN_AS_GID", "1000");
        if let Some(mode) = ownership_mode {
            command.env("FACTORY_WORKSPACE_OWNERSHIP_MODE", mode);
        } else {
            command.env_remove("FACTORY_WORKSPACE_OWNERSHIP_MODE");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "entrypoint {name} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(log).unwrap()
    };

    let managed = run("managed", None);
    assert!(managed.contains("chown -R 1000:1000 /workspaces"));

    let preserved = run("preserved", Some("preserve"));
    assert!(preserved.contains("chown -R 1000:1000 /var/lib/software-factory/codex"));
    assert!(!preserved.contains("chown -R 1000:1000 /workspaces"));
}

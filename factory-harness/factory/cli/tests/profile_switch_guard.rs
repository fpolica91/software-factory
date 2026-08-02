use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde_json::Value;
use serde_json::json;

const NEW_SECRET: &str = "profile-guard-new-secret-must-not-print";
const OLD_SECRET: &str = "profile-guard-old-secret-must-not-print";

#[test]
fn profile_switch_refuses_mismatched_and_unpinned_jobs_without_printing_keys() {
    let config = TestConfig::configured("openai", "gpt-5.6-sol", "OPENAI_API_KEY", OLD_SECRET);
    let original = std::fs::read(&config.path).unwrap();
    let (url, server) = active_jobs_server(vec![
        active_job(
            "job-old-profile",
            "running",
            Some(("openai", "gpt-5.6-sol")),
        ),
        active_job("job-legacy", "cancelling", None),
    ]);

    let output = configure(&config.path, &url, &["--provider", "deepseek"]);

    assert!(!output.status.success());
    let text = combined_output(&output);
    assert!(text.contains("job-old-profile [running]: openai / gpt-5.6-sol"));
    assert!(text.contains("job-legacy [cancelling]: legacy profile is unpinned or invalid"));
    assert!(text.contains("factory configure --provider openai --model gpt-5.6-sol"));
    assert!(text.contains("factory status job-old-profile"));
    assert!(text.contains("factory stop job-legacy"));
    assert!(text.contains("--force"));
    assert_no_secrets(&text);
    assert_eq!(std::fs::read(&config.path).unwrap(), original);
    server.join().unwrap();
}

#[test]
fn force_switch_warns_and_persists_requested_profile_without_printing_key() {
    let config = TestConfig::configured("openai", "gpt-5.6-sol", "OPENAI_API_KEY", OLD_SECRET);
    let (url, server) = active_jobs_server(vec![active_job(
        "job-old-profile",
        "queued",
        Some(("openai", "gpt-5.6-sol")),
    )]);

    let output = configure(
        &config.path,
        &url,
        &[
            "--provider",
            "deepseek",
            "--model",
            "deepseek-v4-flash",
            "--force",
        ],
    );

    assert!(output.status.success(), "{}", combined_output(&output));
    let text = combined_output(&output);
    assert!(text.contains("Warning: forcing provider/model switch"));
    assert!(text.contains("job-old-profile"));
    assert_no_secrets(&text);
    let persisted = std::fs::read_to_string(&config.path).unwrap();
    assert!(persisted.contains("FACTORY_PROVIDER_ADAPTER=\"deepseek\""));
    assert!(persisted.contains("FACTORY_MODEL=\"deepseek-v4-flash\""));
    assert!(persisted.contains(NEW_SECRET));
    server.join().unwrap();
}

#[test]
fn switch_succeeds_when_every_active_job_matches_the_requested_profile() {
    let config = TestConfig::configured("openai", "gpt-5.6-sol", "OPENAI_API_KEY", OLD_SECRET);
    let (url, server) = active_jobs_server(vec![active_job(
        "job-requested-profile",
        "running",
        Some(("deepseek", "deepseek-v4-flash")),
    )]);

    let output = configure(
        &config.path,
        &url,
        &["--provider", "deepseek", "--model", "deepseek-v4-flash"],
    );

    assert!(output.status.success(), "{}", combined_output(&output));
    let text = combined_output(&output);
    assert!(!text.contains("Warning:"));
    assert_no_secrets(&text);
    server.join().unwrap();
}

#[test]
fn initial_configuration_succeeds_without_factoryd_and_does_not_print_key() {
    let config = TestConfig::empty();

    let output = configure(
        &config.path,
        "http://127.0.0.1:9",
        &["--provider", "openai", "--model", "gpt-5.6-sol"],
    );

    assert!(output.status.success(), "{}", combined_output(&output));
    let text = combined_output(&output);
    assert_no_secrets(&text);
    let persisted = std::fs::read_to_string(&config.path).unwrap();
    assert!(persisted.contains("FACTORY_PROVIDER_ADAPTER=\"openai\""));
    assert!(persisted.contains(NEW_SECRET));
}

#[test]
fn initial_configuration_respects_reachable_legacy_jobs() {
    let config = TestConfig::empty();
    let (url, server) = active_jobs_server(vec![active_job("job-retained-legacy", "queued", None)]);

    let output = configure(
        &config.path,
        &url,
        &["--provider", "openai", "--model", "gpt-5.6-sol"],
    );

    assert!(!output.status.success());
    let text = combined_output(&output);
    assert!(text.contains("job-retained-legacy"));
    assert!(text.contains("unknown; not guessed"));
    assert_no_secrets(&text);
    assert!(!config.path.exists());
    server.join().unwrap();
}

fn configure(config: &Path, factoryd_url: &str, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_factory"));
    command
        .args(["--factoryd-url", factoryd_url, "--config-file"])
        .arg(config)
        .arg("configure")
        .args(arguments)
        .args(["--api-key", NEW_SECRET]);
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "DEEPSEEK_API_KEY",
        "ZAI_API_KEY",
    ] {
        command.env_remove(name);
    }
    command.output().unwrap()
}

fn active_jobs_server(jobs: Vec<Value>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        assert!(request.starts_with("GET /jobs/active HTTP/1.1\r\n"));
        let body = serde_json::to_string(&jobs).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        stream.flush().unwrap();
    });
    (format!("http://{address}"), server)
}

fn active_job(id: &str, state: &str, profile: Option<(&str, &str)>) -> Value {
    let execution_profile = profile.map(|(provider, model)| {
        json!({
            "provider": provider,
            "model": model,
        })
    });
    json!({
        "job": {
            "jobId": id,
            "kind": "factory.task",
            "input": {
                "task": "fixture",
                "executionProfile": execution_profile,
            },
            "state": state,
            "createdAt": "2026-08-02T00:00:00Z",
            "updatedAt": "2026-08-02T00:00:01Z",
        },
        "operations": [],
    })
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_secrets(output: &str) {
    assert!(!output.contains(NEW_SECRET), "new key appeared in output");
    assert!(!output.contains(OLD_SECRET), "old key appeared in output");
}

struct TestConfig {
    path: PathBuf,
}

impl TestConfig {
    fn empty() -> Self {
        Self { path: temp_path() }
    }

    fn configured(provider: &str, model: &str, key_name: &str, key: &str) -> Self {
        let path = temp_path();
        std::fs::write(
            &path,
            format!(
                "FACTORY_PROVIDER_ADAPTER=\"{provider}\"\nFACTORY_MODEL=\"{model}\"\n{key_name}=\"{key}\"\nFACTORY_OPENAI_BASE_URL=\"https://api.openai.com/v1\"\n"
            ),
        )
        .unwrap();
        Self { path }
    }
}

impl Drop for TestConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn temp_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "factory-profile-guard-test-{}-{stamp}.env",
        std::process::id()
    ))
}

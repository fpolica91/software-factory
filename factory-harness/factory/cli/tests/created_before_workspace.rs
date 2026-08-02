use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

const JOB_ID: &str = "job-visible-before-workspace";
const MODEL: &str = "model $TOKEN-${OTHER} # \"quoted\" path\\ending";
const REPOSITORY: &str = "https://example.invalid/fixture-repository.git";

#[test]
fn created_job_id_is_flushed_before_workspace_request_completes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (workspace_started_tx, workspace_started_rx) = mpsc::channel();
    let (release_workspace_tx, release_workspace_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut create, _) = listener.accept().unwrap();
        let create_request = read_request(&mut create);
        let definition: serde_json::Value =
            serde_json::from_slice(request_body(&create_request)).unwrap();
        assert_eq!(
            definition["input"]["executionProfile"]["provider"],
            "openai"
        );
        assert_eq!(definition["input"]["executionProfile"]["model"], MODEL);
        let repository_id = definition["input"]["repositoryId"]
            .as_str()
            .unwrap()
            .to_string();
        write_json(&mut create, &created_job_json());

        let (mut workspace, _) = listener.accept().unwrap();
        let workspace_request = read_request(&mut workspace);
        let workspace_request: serde_json::Value =
            serde_json::from_slice(request_body(&workspace_request)).unwrap();
        assert_eq!(workspace_request["repositoryId"], repository_id);
        assert_eq!(workspace_request["repository"], REPOSITORY);
        assert_eq!(workspace_request["baseRef"], "main");
        workspace_started_tx.send(()).unwrap();
        release_workspace_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        write_json(&mut workspace, &workspace_json(&repository_id));
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_factory"))
        .args([
            "--factoryd-url",
            &format!("http://{address}"),
            "run",
            "--detach",
            "--json",
            "--repository",
            REPOSITORY,
            "--base-ref",
            "main",
            "fixture task",
        ])
        .env("FACTORY_PROVIDER_ADAPTER", "openai")
        .env("FACTORY_MODEL", MODEL)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (output_tx, output_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut first = String::new();
        stdout.read_line(&mut first).unwrap();
        output_tx.send(first).unwrap();
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        rest
    });

    workspace_started_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let first = output_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    assert_eq!(first["type"], "created");
    assert_eq!(first["jobId"], JOB_ID);

    release_workspace_tx.send(()).unwrap();
    let status = child.wait().unwrap();
    let rest = reader.join().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "factory failed: {stderr}");
    assert!(rest.contains(r#""type":"workspaceReady""#));
    server.join().unwrap();
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        request.extend_from_slice(&buffer[..count]);
        let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let header_end = header_end + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap_or(0);
        if request.len() >= header_end + content_length {
            return request;
        }
    }
}

fn request_body(request: &[u8]) -> &[u8] {
    let header_end = request
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    &request[header_end..]
}

fn write_json(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}

fn created_job_json() -> String {
    format!(
        r#"{{"job":{{"jobId":"{JOB_ID}","kind":"factory.task","input":{{}},"state":"queued","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z"}},"operations":[]}}"#
    )
}

fn workspace_json(repository_id: &str) -> String {
    format!(
        r#"{{"jobId":"{JOB_ID}","repositoryId":"{repository_id}","repository":"{REPOSITORY}","baseRef":"main","baseRevision":"abc","branchName":"factory/{JOB_ID}","root":"/workspace/{JOB_ID}","revision":"abc","state":"active","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z"}}"#
    )
}

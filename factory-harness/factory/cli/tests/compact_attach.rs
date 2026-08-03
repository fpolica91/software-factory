use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

const JOB_ID: &str = "job-compact-attach";

#[test]
fn non_tty_attach_collapses_lifecycle_noise_and_keeps_the_result() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().unwrap();
            let path = read_request_path(&mut stream);
            let body = if path == format!("/jobs/{JOB_ID}") {
                terminal_job_json()
            } else if path.ends_with("/stage-checkpoints") || path.ends_with("/attempts") {
                "[]".to_string()
            } else if path.contains("/events?after=7") {
                r#"{"events":[],"nextCursor":7}"#.to_string()
            } else if path.contains("/events?after=0") {
                event_page_json()
            } else {
                panic!("unexpected request: {path}")
            };
            write_json(&mut stream, &body);
        }
    });

    let output = Command::new(env!("CARGO_BIN_EXE_factory"))
        .args([
            "--factoryd-url",
            &format!("http://{address}"),
            "attach",
            JOB_ID,
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "factory attach failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.lines().count() < 20, "compact output grew: {stdout}");
    assert!(!stdout.contains("tool.started"), "{stdout}");
    assert!(!stdout.contains("tool.completed"), "{stdout}");
    assert!(!stdout.contains("cargo check --workspace"), "{stdout}");
    assert_eq!(stdout.matches("Audit finished.").count(), 1, "{stdout}");
    assert!(stdout.contains("provider will retry"), "{stdout}");
    assert!(stdout.contains("Result: succeeded"), "{stdout}");
    assert!(stdout.contains("# Result"), "{stdout}");
    assert!(stdout.contains("## Execute"), "{stdout}");
    assert!(stdout.contains("Inspect: factory result"), "{stdout}");

    server.join().unwrap();
}

fn read_request_path(stream: &mut TcpStream) -> String {
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
    String::from_utf8_lossy(&request)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string()
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

fn terminal_job_json() -> String {
    format!(
        r#"{{"job":{{"jobId":"{JOB_ID}","kind":"factory.task","input":{{}},"state":"succeeded","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:01Z"}},"operations":[{{"operationId":"operation-execute","jobId":"{JOB_ID}","ordinal":1,"kind":"codex.execute","input":{{}},"state":"succeeded","maxAttempts":3,"nextEligibleAt":"2026-08-02T00:00:00Z","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:01Z"}}]}}"#
    )
}

fn event_page_json() -> String {
    format!(
        r#"{{"events":[
            {{"sequence":1,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"tool.started","payload":{{"threadId":"thread-1","turnId":"turn-1","itemId":"tool-1","type":"command","message":"cargo check --workspace","status":"inProgress"}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":2,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"tool.completed","payload":{{"threadId":"thread-1","turnId":"turn-1","itemId":"tool-1","type":"command","message":"cargo check --workspace","status":"completed","exitCode":0}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":3,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"agent.message","payload":{{"threadId":"thread-1","turnId":"turn-1","itemId":"answer-1","partIndex":null,"text":"Audit "}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":4,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"agent.message","payload":{{"threadId":"thread-1","turnId":"turn-1","itemId":"answer-1","partIndex":null,"text":"finished."}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":5,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"agent.message.completed","payload":{{"threadId":"thread-1","turnId":"turn-1","itemId":"answer-1","phase":null}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":6,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"turn.warning","payload":{{"threadId":"thread-1","turnId":"turn-1","message":"provider will retry"}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":7,"jobId":"{JOB_ID}","operationId":"operation-execute","attemptId":"attempt-1","kind":"stage.completed","payload":{{"stage":"codex.execute","role":"stage","reviewCycle":0,"threadId":"thread-1","turnId":"turn-1","findings":[]}},"createdAt":"2026-08-02T00:00:01Z"}}
        ],"nextCursor":7}}"#
    )
}

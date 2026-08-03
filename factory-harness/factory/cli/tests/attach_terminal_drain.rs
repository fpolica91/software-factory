use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

const JOB_ID: &str = "job-terminal-race";

#[test]
fn attach_drains_atomic_completion_event_after_observing_terminal_state() {
    let artifact_root = std::env::temp_dir().join(format!(
        "factory-attach-terminal-artifacts-{}",
        std::process::id()
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        let mut event_request = 0;
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().unwrap();
            let path = read_request_path(&mut stream);
            paths.push(path.clone());
            let body = if path == format!("/jobs/{JOB_ID}") {
                terminal_job_json()
            } else if path.ends_with("/stage-checkpoints") || path.ends_with("/attempts") {
                "[]".to_string()
            } else if path.starts_with(&format!("/jobs/{JOB_ID}/events?")) {
                let body = match event_request {
                    0 => r#"{"events":[],"nextCursor":0}"#.to_string(),
                    1 => final_event_page_json(),
                    2 => reconstruction_event_page_json(),
                    _ => panic!("unexpected events request {event_request}: {path}"),
                };
                event_request += 1;
                body
            } else {
                panic!("unexpected request: {path}")
            };
            write_json(&mut stream, &body);
        }
        paths
    });

    let output = Command::new(env!("CARGO_BIN_EXE_factory"))
        .args([
            "--factoryd-url",
            &format!("http://{address}"),
            "attach",
            "--json",
            JOB_ID,
        ])
        .env("FACTORY_ARTIFACT_ROOT", &artifact_root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "factory attach failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    let lines = lines.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "unexpected attach output: {lines:?}");
    let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let result: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(event["type"], "event");
    assert_eq!(event["event"]["kind"], "stage.completed");
    assert_eq!(result["type"], "result");
    assert_eq!(result["job"]["job"]["state"], "succeeded");
    assert_eq!(
        result["fullResult"]["markdown"],
        "# Result\n\n## Review\n\nReview completed.\n"
    );
    let rendered = artifact_root.join("coordinator/jobs").join(JOB_ID);
    assert!(rendered.join("job.json").is_file());
    assert!(rendered.join("task.md").is_file());

    let paths = server.join().unwrap();
    assert!(paths[0].contains("/events?after=0"));
    assert_eq!(paths[1], format!("/jobs/{JOB_ID}"));
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.starts_with(&format!("/jobs/{JOB_ID}/events?")))
            .count(),
        3
    );
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.as_str() == format!("/jobs/{JOB_ID}"))
            .count(),
        2
    );
    std::fs::remove_dir_all(artifact_root).unwrap();
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
    let first_line = String::from_utf8_lossy(&request)
        .lines()
        .next()
        .unwrap()
        .to_string();
    first_line.split_whitespace().nth(1).unwrap().to_string()
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
        r#"{{"job":{{"jobId":"{JOB_ID}","kind":"factory.task","input":{{}},"state":"succeeded","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:01Z"}},"operations":[{{"operationId":"operation-review","jobId":"{JOB_ID}","ordinal":1,"kind":"codex.review","input":{{}},"state":"succeeded","maxAttempts":3,"nextEligibleAt":"2026-08-02T00:00:00Z","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:01Z"}}]}}"#
    )
}

fn final_event_page_json() -> String {
    format!(
        r#"{{"events":[{{"sequence":2,"jobId":"{JOB_ID}","operationId":"operation-review","attemptId":"attempt-review","kind":"stage.completed","payload":{{"stage":"codex.review","role":"stage","reviewCycle":0,"turnId":"turn-review","findings":[]}},"createdAt":"2026-08-02T00:00:01Z"}}],"nextCursor":2}}"#
    )
}

fn reconstruction_event_page_json() -> String {
    format!(
        r#"{{"events":[
            {{"sequence":1,"jobId":"{JOB_ID}","operationId":"operation-review","attemptId":"attempt-review","kind":"agent.message.completed","payload":{{"turnId":"turn-review","itemId":"answer-review","phase":"final_answer","text":"Review completed."}},"createdAt":"2026-08-02T00:00:00Z"}},
            {{"sequence":2,"jobId":"{JOB_ID}","operationId":"operation-review","attemptId":"attempt-review","kind":"stage.completed","payload":{{"stage":"codex.review","role":"stage","reviewCycle":0,"turnId":"turn-review","findings":[]}},"createdAt":"2026-08-02T00:00:01Z"}}
        ],"nextCursor":2}}"#
    )
}

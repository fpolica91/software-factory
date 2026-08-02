use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

const JOB_ID: &str = "job-terminal-race";

#[test]
fn attach_drains_atomic_completion_event_after_observing_terminal_state() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        for request_number in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let path = read_request_path(&mut stream);
            paths.push(path.clone());
            let body = match request_number {
                0 => r#"{"events":[],"nextCursor":0}"#.to_string(),
                1 => terminal_job_json(),
                2 => final_event_page_json(),
                _ if path.ends_with("/stage-checkpoints") || path.ends_with("/attempts") => {
                    "[]".to_string()
                }
                _ => panic!("unexpected request {request_number}: {path}"),
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

    let paths = server.join().unwrap();
    assert!(paths[0].contains("/events?after=0"));
    assert_eq!(paths[1], format!("/jobs/{JOB_ID}"));
    assert!(paths[2].contains("/events?after=0"));
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
        r#"{{"job":{{"jobId":"{JOB_ID}","kind":"factory.task","input":{{}},"state":"succeeded","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:01Z"}},"operations":[]}}"#
    )
}

fn final_event_page_json() -> String {
    format!(
        r#"{{"events":[{{"sequence":1,"jobId":"{JOB_ID}","operationId":null,"attemptId":null,"kind":"stage.completed","payload":{{"stage":"codex.review"}},"createdAt":"2026-08-02T00:00:01Z"}}],"nextCursor":1}}"#
    )
}

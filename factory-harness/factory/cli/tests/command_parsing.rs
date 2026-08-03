use std::process::Command;

#[test]
fn result_and_artifacts_reach_their_native_subcommands() {
    for (command, description) in [
        ("result", "Print the complete durable human result"),
        ("artifacts", "List the fixed artifacts for a durable job"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_factory"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{command} --help failed");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains(description),
            "unexpected {command} help:\n{stdout}"
        );
        assert!(
            stdout.contains("<JOB_ID>"),
            "missing job id in {command} help"
        );
    }
}

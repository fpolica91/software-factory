use anyhow::Result;
use codex_agent_extension::AgentHistoryPolicy;
use codex_agent_extension::AgentInvocation;
use codex_agent_extension::AgentRunner;
use codex_agent_extension::AgentStartOptions;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_resolved_agent_prompt_in_forked_thread() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("agent-response"),
            responses::ev_completed("agent-response"),
        ]),
    )
    .await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let parent_thread_id = test.session_configured.session_id.into();
    let agent_runner = AgentRunner::new(std::sync::Arc::downgrade(&test.thread_manager));

    let agent_run = agent_runner
        .start(
            parent_thread_id,
            AgentInvocation {
                config: test.config.clone(),
                prompt: "Use $example-agent to inspect the current changes.".to_string(),
                parent_trace: None,
            },
        )
        .await?;

    assert_ne!(agent_run.thread_id, parent_thread_id);
    assert_eq!(
        agent_run
            .thread
            .config_snapshot()
            .await
            .forked_from_thread_id,
        Some(parent_thread_id)
    );
    let started = wait_for_event(&agent_run.thread, |event| {
        matches!(event, EventMsg::TurnStarted(_))
    })
    .await;
    let EventMsg::TurnStarted(started) = started else {
        unreachable!("event predicate only matches turn started events");
    };
    assert_eq!(started.turn_id, agent_run.turn_id);
    wait_for_event(&agent_run.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let request = response_mock.single_request();
    assert!(
        request
            .message_input_texts("user")
            .iter()
            .any(|text| text == "Use $example-agent to inspect the current changes.")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_resolved_agent_with_fresh_history_when_requested() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const PARENT_MARKER: &str = "PARENT_HISTORY_MUST_NOT_REACH_FRESH_AGENT";
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("parent-response"),
                responses::ev_assistant_message("parent-message", "parent complete"),
                responses::ev_completed("parent-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("agent-response"),
                responses::ev_completed("agent-response"),
            ]),
        ],
    )
    .await;
    let test = test_codex().build_with_auto_env(&server).await?;
    test.submit_turn(PARENT_MARKER).await?;
    let parent_thread_id = test.session_configured.session_id.into();
    let agent_runner = AgentRunner::new(std::sync::Arc::downgrade(&test.thread_manager));

    let agent_run = agent_runner
        .start_with_options(
            parent_thread_id,
            AgentInvocation {
                config: test.config.clone(),
                prompt: "Review the current changes without parent conversation.".to_string(),
                parent_trace: None,
            },
            AgentStartOptions {
                history_policy: AgentHistoryPolicy::Fresh,
                ..AgentStartOptions::default()
            },
        )
        .await?;

    assert_ne!(agent_run.thread_id, parent_thread_id);
    assert_eq!(
        agent_run
            .thread
            .config_snapshot()
            .await
            .forked_from_thread_id,
        None
    );
    wait_for_event(&agent_run.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].body_contains_text(PARENT_MARKER));

    Ok(())
}

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use factory_providers::AdapterKind;
use factory_providers::ProviderProfile;
use factory_providers::provider_profile;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use crate::config::prompt_line;
use crate::config::upstream_base_variable;
use crate::required_env;

const MAX_QUESTIONS: usize = 5;
const GATE_TIMEOUT: Duration = Duration::from_secs(90);
const GATE_MAX_TOKENS: u32 = 1024;

const GATE_INSTRUCTION: &str = "You are the intake gate for an autonomous software agent. \
The agent cannot ask questions once it starts, so ambiguity must be resolved now. \
Read the task below. If it is specific enough to implement without guessing at intent, \
reply with exactly NONE. Otherwise reply with at most 5 numbered clarifying questions \
(format: `1. question`), one per line, with no preamble and no other text. Only ask \
questions whose answers would change what gets built.";

/// Interactive pre-submission clarification gate.
///
/// Asks the configured provider whether the task is ambiguous; when it is,
/// collects answers on the terminal, appends them to the task, and records the
/// submitted prompt under `.factory/prompts/`. Fail-open by design: any
/// provider or I/O failure submits the original task unchanged.
pub(crate) async fn gate(task: String, skip: bool, local_root: Option<&Path>) -> String {
    if skip {
        return task;
    }
    let reply = match ask_gate_model(&task).await {
        Ok(reply) => reply,
        Err(error) => {
            eprintln!("clarification gate skipped: {error:#}");
            return task;
        }
    };
    let Some(questions) = parse_questions(&reply) else {
        return task;
    };
    let answers = match collect_answers(&questions) {
        Ok(answers) => answers,
        Err(error) => {
            eprintln!("clarification gate skipped: {error:#}");
            return task;
        }
    };
    let task = compose_task(&task, &questions, &answers);
    if let Some(root) = local_root {
        match write_prompt_file(root, &task) {
            Ok(path) => eprintln!("Clarified prompt saved to {}", path.display()),
            Err(error) => eprintln!("clarified prompt file was not written: {error:#}"),
        }
    }
    task
}

async fn ask_gate_model(task: &str) -> Result<String> {
    let provider = required_env("FACTORY_PROVIDER_ADAPTER")?;
    let profile = provider_profile(&provider)
        .ok_or_else(|| anyhow!("unknown provider profile {provider:?}"))?;
    let model = required_env("FACTORY_MODEL")?;
    let key = required_env(profile.api_key_env)?;
    let base = std::env::var(upstream_base_variable(profile))
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| profile.base_urls[0].url.trim_end_matches('/').to_string());
    ask_provider(profile, &base, &key, &model, task).await
}

async fn ask_provider(
    profile: &ProviderProfile,
    base: &str,
    key: &str,
    model: &str,
    task: &str,
) -> Result<String> {
    let prompt = format!("{GATE_INSTRUCTION}\n\nTask:\n{task}");
    let client = reqwest::Client::builder()
        .timeout(GATE_TIMEOUT)
        .build()
        .context("build clarification gate HTTP client")?;
    let request = match profile.adapter_kind {
        AdapterKind::ChatCompletions => client
            .post(format!("{base}/chat/completions"))
            .bearer_auth(key)
            .json(&json!({
                "model": model,
                "max_tokens": GATE_MAX_TOKENS,
                "stream": false,
                "messages": [{"role": "user", "content": prompt}],
            })),
        AdapterKind::AnthropicMessages => client
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": model,
                "max_tokens": GATE_MAX_TOKENS,
                "messages": [{"role": "user", "content": prompt}],
            })),
        AdapterKind::DirectResponses => client
            .post(format!("{base}/responses"))
            .bearer_auth(key)
            .json(&json!({
                "model": model,
                "input": prompt,
            })),
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("reach {} clarification endpoint", profile.label))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("read {} clarification response", profile.label))?;
    if !status.is_success() {
        return Err(anyhow!(
            "{} clarification request failed with {status}: {}",
            profile.label,
            truncate(&body, 300)
        ));
    }
    let body: Value = serde_json::from_str(&body)
        .with_context(|| format!("parse {} clarification response", profile.label))?;
    let text = extract_text(profile.adapter_kind, &body)
        .ok_or_else(|| anyhow!("{} clarification response had no text", profile.label))?;
    Ok(text)
}

fn extract_text(kind: AdapterKind, body: &Value) -> Option<String> {
    match kind {
        AdapterKind::ChatCompletions => body["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string),
        AdapterKind::AnthropicMessages => body["content"].as_array().and_then(|blocks| {
            blocks
                .iter()
                .find(|block| block["type"] == "text")
                .and_then(|block| block["text"].as_str())
                .map(str::to_string)
        }),
        AdapterKind::DirectResponses => {
            let output = body["output"].as_array()?;
            let text = output
                .iter()
                .filter(|item| item["type"] == "message")
                .filter_map(|item| item["content"].as_array())
                .flatten()
                .filter(|block| block["type"] == "output_text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
    }
}

fn parse_questions(reply: &str) -> Option<Vec<String>> {
    let trimmed = reply.trim();
    if trimmed
        .trim_start_matches(['`', '*', '_'])
        .to_ascii_uppercase()
        .starts_with("NONE")
    {
        return None;
    }
    let mut questions = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim_start();
        let Some(question) = strip_number_prefix(line) else {
            continue;
        };
        if !question.is_empty() {
            questions.push(question.to_string());
        }
        if questions.len() == MAX_QUESTIONS {
            break;
        }
    }
    (!questions.is_empty()).then_some(questions)
}

fn strip_number_prefix(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = line[digits..]
        .strip_prefix('.')
        .or_else(|| line[digits..].strip_prefix(')'))?;
    Some(rest.trim())
}

fn collect_answers(questions: &[String]) -> Result<Vec<String>> {
    eprintln!();
    eprintln!(
        "The task needs clarification before it is submitted. Enter leaves a question to the agent's judgment."
    );
    let mut answers = Vec::with_capacity(questions.len());
    for (index, question) in questions.iter().enumerate() {
        eprintln!();
        eprintln!("{}. {question}", index + 1);
        answers.push(prompt_line("> ")?);
    }
    eprintln!();
    Ok(answers)
}

fn compose_task(task: &str, questions: &[String], answers: &[String]) -> String {
    let mut composed = format!("{}\n\n## Clarifications\n", task.trim_end());
    for (question, answer) in questions.iter().zip(answers) {
        let answer = if answer.is_empty() {
            "Use your judgment."
        } else {
            answer.as_str()
        };
        composed.push_str(&format!("\nQ: {question}\nA: {answer}\n"));
    }
    composed
}

fn write_prompt_file(root: &Path, task: &str) -> Result<PathBuf> {
    let digest = Sha256::digest(task.as_bytes());
    let id = format!("{digest:x}");
    let directory = root.join(".factory").join("prompts");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create {}", directory.display()))?;
    let path = directory.join(format!("prompt_{}.md", &id[..12]));
    std::fs::write(&path, task).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn truncate(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_replies_raise_no_questions() {
        assert_eq!(parse_questions("NONE"), None);
        assert_eq!(parse_questions("none"), None);
        assert_eq!(parse_questions(" None. "), None);
        assert_eq!(parse_questions("**NONE**"), None);
        assert_eq!(parse_questions("The task is clear and ready."), None);
        assert_eq!(parse_questions(""), None);
    }

    #[test]
    fn numbered_questions_are_extracted_in_order() {
        let reply = "1. Which auth flow?\n2) Postgres or SQLite?\n- 3. Ignore casing?";
        assert_eq!(
            parse_questions(reply),
            Some(vec![
                "Which auth flow?".to_string(),
                "Postgres or SQLite?".to_string(),
                "Ignore casing?".to_string(),
            ])
        );
    }

    #[test]
    fn question_count_is_capped() {
        let reply = (1..=8)
            .map(|index| format!("{index}. Question {index}?"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_questions(&reply).unwrap().len(), MAX_QUESTIONS);
    }

    #[test]
    fn composed_task_keeps_original_and_defaults_blank_answers() {
        let composed = compose_task(
            "implement abc\n",
            &["Which abc?".to_string(), "Where?".to_string()],
            &["the parser".to_string(), String::new()],
        );
        assert_eq!(
            composed,
            "implement abc\n\n## Clarifications\n\nQ: Which abc?\nA: the parser\n\nQ: Where?\nA: Use your judgment.\n"
        );
    }

    #[test]
    fn prompt_file_lands_under_factory_prompts_with_content_hash_id() {
        let root = std::env::temp_dir().join(format!("factory-clarify-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = write_prompt_file(&root, "task text").unwrap();
        assert!(path.starts_with(root.join(".factory").join("prompts")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "task text");
        let again = write_prompt_file(&root, "task text").unwrap();
        assert_eq!(path, again);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn provider_response_text_is_extracted_per_adapter() {
        let chat = json!({"choices": [{"message": {"content": "NONE"}}]});
        assert_eq!(
            extract_text(AdapterKind::ChatCompletions, &chat).as_deref(),
            Some("NONE")
        );
        let anthropic = json!({"content": [
            {"type": "thinking", "thinking": "..."},
            {"type": "text", "text": "1. Which abc?"}
        ]});
        assert_eq!(
            extract_text(AdapterKind::AnthropicMessages, &anthropic).as_deref(),
            Some("1. Which abc?")
        );
        let responses = json!({"output": [
            {"type": "reasoning", "summary": []},
            {"type": "message", "content": [{"type": "output_text", "text": "NONE"}]}
        ]});
        assert_eq!(
            extract_text(AdapterKind::DirectResponses, &responses).as_deref(),
            Some("NONE")
        );
    }

    #[tokio::test]
    #[ignore = "live provider call; requires DEEPSEEK_API_KEY and network"]
    async fn live_deepseek_gate_questions_a_vague_task() {
        let key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY is required");
        let profile = provider_profile("deepseek").expect("deepseek profile exists");
        let reply = ask_provider(
            profile,
            "https://api.deepseek.com",
            &key,
            "deepseek-v4-flash",
            "make it better",
        )
        .await
        .expect("deepseek gate call succeeds");
        assert!(
            parse_questions(&reply).is_some(),
            "a vague task should produce numbered questions, got: {reply}"
        );
    }
}

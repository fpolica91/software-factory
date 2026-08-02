use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use serde_json::json;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::anthropic;
use crate::anthropic::AnthropicStreamTranslator;
use crate::chat;
use crate::chat::ChatStreamTranslator;
use crate::config::AdapterConfig;
use crate::config::write_model_catalog;
use crate::profiles::AdapterKind;
use crate::profiles::CodexProviderSelection;
use crate::responses::TranslationError;
use crate::responses::failed_event;
use crate::sse;

pub struct ProviderAdapter {
    listener: TcpListener,
    state: Arc<ServerState>,
    selection: CodexProviderSelection,
}

impl ProviderAdapter {
    pub async fn bind(mut config: AdapterConfig) -> Result<Self, AdapterError> {
        let listener = TcpListener::bind((config.bind_host.as_str(), config.port)).await?;
        let address = listener.local_addr()?;
        if config.port == 0 {
            config.port = address.port();
            config.advertised_base_url = format!("http://127.0.0.1:{}/v1", address.port());
        }
        let catalog = write_model_catalog(&config).await?;
        let selection = CodexProviderSelection::for_profile(
            config.profile,
            &config.advertised_base_url,
            &config.model,
            Some(catalog),
        );
        let state = Arc::new(ServerState {
            client: Client::builder().build()?,
            config,
        });
        Ok(Self {
            listener,
            state,
            selection,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.state.config.advertised_base_url
    }

    pub fn selection(&self) -> &CodexProviderSelection {
        &self.selection
    }

    pub async fn run(self) -> Result<(), AdapterError> {
        let app = Router::new()
            .route("/healthz", get(health))
            .route("/v1/responses", post(responses))
            .with_state(self.state);
        axum::serve(self.listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok(())
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;

        let terminate = tokio::signal::unix::signal(SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("provider adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider adapter HTTP client failed: {0}")]
    Http(#[from] reqwest::Error),
}

struct ServerState {
    client: Client,
    config: AdapterConfig,
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn responses(State(state): State<Arc<ServerState>>, Json(request): Json<Value>) -> Response {
    match forward_responses(state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn forward_responses(
    state: Arc<ServerState>,
    request: Value,
) -> Result<Response, HandlerError> {
    if request.get("stream").and_then(Value::as_bool) != Some(true) {
        return Err(HandlerError::BadRequest(
            "Factory provider adapters require stream=true".to_string(),
        ));
    }
    let requested_model = request.get("model").and_then(Value::as_str).unwrap_or("");
    if requested_model != state.config.model {
        return Err(HandlerError::BadRequest(format!(
            "adapter is configured for model {}, not {requested_model}",
            state.config.model
        )));
    }

    let (body, translator, url) = match state.config.profile.adapter_kind {
        AdapterKind::ChatCompletions => {
            let prepared = chat::prepare_request(&request, &state.config)?;
            (
                prepared.body,
                Translator::Chat(ChatStreamTranslator::new(
                    &state.config.model,
                    prepared.tools,
                )),
                format!("{}/chat/completions", state.config.upstream_base_url),
            )
        }
        AdapterKind::AnthropicMessages => {
            let prepared = anthropic::prepare_request(&request, &state.config)?;
            (
                prepared.body,
                Translator::Anthropic(AnthropicStreamTranslator::new(
                    &state.config.model,
                    prepared.tools,
                )),
                anthropic_messages_url(&state.config.upstream_base_url),
            )
        }
        AdapterKind::DirectResponses => {
            return Err(HandlerError::BadRequest(
                "direct Responses providers must bypass this adapter".to_string(),
            ));
        }
    };

    let mut upstream_request = state.client.post(url).json(&body);
    upstream_request = match state.config.profile.adapter_kind {
        AdapterKind::AnthropicMessages => upstream_request
            .header("x-api-key", &state.config.api_key)
            .header("anthropic-version", "2023-06-01"),
        AdapterKind::ChatCompletions => upstream_request.bearer_auth(&state.config.api_key),
        AdapterKind::DirectResponses => unreachable!(),
    };
    let upstream = upstream_request
        .send()
        .await
        .map_err(HandlerError::Upstream)?;
    if !upstream.status().is_success() {
        let status = upstream.status();
        let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
        let bytes = upstream.bytes().await.map_err(HandlerError::Upstream)?;
        let mut builder = Response::builder().status(status);
        if let Some(content_type) = content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        return Ok(builder
            .body(Body::from(bytes))
            .expect("valid upstream response"));
    }

    let (sender, receiver) = mpsc::channel::<Result<bytes::Bytes, Infallible>>(32);
    tokio::spawn(stream_upstream(upstream, translator, sender));
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(ReceiverStream::new(receiver)))
        .expect("valid Responses stream"))
}

pub(crate) fn anthropic_messages_url(upstream_base_url: &str) -> String {
    let base = upstream_base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/v1/messages")
}

enum Translator {
    Chat(ChatStreamTranslator),
    Anthropic(AnthropicStreamTranslator),
}

impl Translator {
    fn created(&self) -> Value {
        match self {
            Self::Chat(translator) => translator.created(),
            Self::Anthropic(translator) => translator.created(),
        }
    }

    fn response_id(&self) -> &str {
        match self {
            Self::Chat(translator) => translator.response_id(),
            Self::Anthropic(translator) => translator.response_id(),
        }
    }

    fn push(&mut self, event: &Value) -> Result<Vec<Value>, TranslationError> {
        match self {
            Self::Chat(translator) => translator.push(event),
            Self::Anthropic(translator) => translator.push(event),
        }
    }

    fn finish(self, saw_end_marker: bool) -> Result<Vec<Value>, TranslationError> {
        match self {
            Self::Chat(translator) => translator.finish(saw_end_marker),
            Self::Anthropic(translator) => translator.finish(),
        }
    }
}

async fn stream_upstream(
    upstream: reqwest::Response,
    mut translator: Translator,
    sender: mpsc::Sender<Result<bytes::Bytes, Infallible>>,
) {
    if sender
        .send(Ok(sse::encode(&translator.created())))
        .await
        .is_err()
    {
        return;
    }
    let mut stream = upstream.bytes_stream().eventsource();
    let mut saw_end_marker = false;
    while let Some(event) = stream.next().await {
        let data = match event {
            Ok(event) if event.data == "[DONE]" => {
                saw_end_marker = true;
                break;
            }
            Ok(event) => event.data,
            Err(error) => {
                send_failure(&sender, translator.response_id(), &error.to_string()).await;
                return;
            }
        };
        let value: Value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(error) => {
                send_failure(&sender, translator.response_id(), &error.to_string()).await;
                return;
            }
        };
        let events = match translator.push(&value) {
            Ok(events) => events,
            Err(error) => {
                send_failure(&sender, translator.response_id(), &error.to_string()).await;
                return;
            }
        };
        for event in events {
            if sender.send(Ok(sse::encode(&event))).await.is_err() {
                return;
            }
        }
    }
    let response_id = translator.response_id().to_string();
    let events = match translator.finish(saw_end_marker) {
        Ok(events) => events,
        Err(error) => {
            send_failure(&sender, &response_id, &error.to_string()).await;
            return;
        }
    };
    for event in events {
        if sender.send(Ok(sse::encode(&event))).await.is_err() {
            return;
        }
    }
    let _ = sender.send(Ok(sse::done())).await;
}

async fn send_failure(
    sender: &mpsc::Sender<Result<bytes::Bytes, Infallible>>,
    response_id: &str,
    message: &str,
) {
    let _ = sender
        .send(Ok(sse::encode(&failed_event(response_id, message))))
        .await;
    let _ = sender.send(Ok(sse::done())).await;
}

enum HandlerError {
    BadRequest(String),
    Translation(TranslationError),
    Upstream(reqwest::Error),
}

impl From<TranslationError> for HandlerError {
    fn from(value: TranslationError) -> Self {
        Self::Translation(value)
    }
}

impl IntoResponse for HandlerError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Translation(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::Upstream(error) => (StatusCode::BAD_GATEWAY, error.to_string()),
        };
        (status, Json(json!({"error": {"message": message}}))).into_response()
    }
}

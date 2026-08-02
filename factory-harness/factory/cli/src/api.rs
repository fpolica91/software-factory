use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use factory_coordinator::AttemptRecord;
use factory_coordinator::DurableJob;
use factory_coordinator::EnsureWorkspaceRequest;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobEventPage;
use factory_coordinator::JobId;
use factory_coordinator::StageCheckpointRecord;
use factory_coordinator::WorkspaceRecord;
use reqwest::Method;
use reqwest::RequestBuilder;
use reqwest::Url;
use serde::de::DeserializeOwned;
use std::time::Duration;

const GENERAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct FactorydClient {
    base_url: Url,
    http: reqwest::Client,
    workspace_http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedResult {
    pub repository_id: String,
    pub base_revision: String,
    pub patch_sha256: String,
    pub patch: Vec<u8>,
}

impl FactorydClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_general_timeout(base_url, GENERAL_REQUEST_TIMEOUT)
    }

    fn with_general_timeout(base_url: &str, general_timeout: Duration) -> Result<Self> {
        let mut base_url = Url::parse(base_url).context("parse factoryd URL")?;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let http = reqwest::Client::builder()
            .timeout(general_timeout)
            .build()
            .context("build factoryd HTTP client")?;
        let workspace_http = reqwest::Client::builder()
            .connect_timeout(general_timeout)
            .build()
            .context("build factoryd workspace HTTP client")?;
        Ok(Self {
            base_url,
            http,
            workspace_http,
        })
    }

    pub async fn create_job(&self, definition: &JobDefinition) -> Result<DurableJob> {
        self.json(self.request(Method::POST, &["jobs"])?.json(definition))
            .await
    }

    pub async fn list_active_jobs(&self) -> Result<Vec<DurableJob>> {
        self.json(self.request(Method::GET, &["jobs", "active"])?)
            .await
    }

    pub async fn ensure_workspace(
        &self,
        job_id: &JobId,
        request: &EnsureWorkspaceRequest,
    ) -> Result<WorkspaceRecord> {
        let url = self.url(&["jobs", job_id.as_str(), "workspace"])?;
        self.json(self.workspace_http.put(url).json(request)).await
    }

    pub async fn load_job(&self, job_id: &JobId) -> Result<DurableJob> {
        self.json(self.request(Method::GET, &["jobs", job_id.as_str()])?)
            .await
    }

    pub async fn cancel_job(&self, job_id: &JobId) -> Result<DurableJob> {
        self.json(self.request(Method::POST, &["jobs", job_id.as_str(), "cancel"])?)
            .await
    }

    pub async fn list_events(
        &self,
        job_id: &JobId,
        after: u64,
        limit: u32,
    ) -> Result<JobEventPage> {
        let mut url = self.url(&["jobs", job_id.as_str(), "events"])?;
        url.query_pairs_mut()
            .append_pair("after", &after.to_string())
            .append_pair("limit", &limit.to_string());
        self.json(self.http.get(url)).await
    }

    pub async fn list_stage_checkpoints(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<StageCheckpointRecord>> {
        self.json(self.request(Method::GET, &["jobs", job_id.as_str(), "stage-checkpoints"])?)
            .await
    }

    pub async fn list_attempts(&self, job_id: &JobId) -> Result<Vec<AttemptRecord>> {
        self.json(self.request(Method::GET, &["jobs", job_id.as_str(), "attempts"])?)
            .await
    }

    pub async fn export_result(&self, job_id: &JobId) -> Result<ExportedResult> {
        let url = self.url(&["jobs", job_id.as_str(), "result"])?;
        let response = self
            .workspace_http
            .get(url)
            .send()
            .await
            .context("contact factoryd for job result")?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .context("read factoryd result error")?;
            return Err(response_error(status, &body));
        }
        let repository_id = response_header(&response, "x-factory-repository-id")?;
        let base_revision = response_header(&response, "x-factory-base-revision")?;
        let patch_sha256 = response_header(&response, "x-factory-patch-sha256")?;
        let patch = response
            .bytes()
            .await
            .context("read factoryd result")?
            .to_vec();
        Ok(ExportedResult {
            repository_id,
            base_revision,
            patch_sha256,
            patch,
        })
    }

    fn request(&self, method: Method, segments: &[&str]) -> Result<RequestBuilder> {
        Ok(self.http.request(method, self.url(segments)?))
    }

    fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| anyhow!("factoryd URL cannot be a base URL"))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let response = request.send().await.context("contact factoryd")?;
        let status = response.status();
        let body = response.bytes().await.context("read factoryd response")?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<ErrorEnvelope>(&body)
                .ok()
                .map(|value| value.error.message)
                .or_else(|| {
                    let text = String::from_utf8_lossy(&body).trim().to_string();
                    (!text.is_empty()).then_some(text)
                })
                .unwrap_or_else(|| "empty response".to_string());
            return Err(anyhow!("factoryd returned {status}: {detail}"));
        }
        serde_json::from_slice(&body).context("decode factoryd response")
    }
}

fn response_header(response: &reqwest::Response, name: &'static str) -> Result<String> {
    response
        .headers()
        .get(name)
        .ok_or_else(|| anyhow!("factoryd result is missing {name}"))?
        .to_str()
        .with_context(|| format!("factoryd result has invalid {name}"))
        .map(str::to_string)
}

fn response_error(status: reqwest::StatusCode, body: &[u8]) -> anyhow::Error {
    let detail = serde_json::from_slice::<ErrorEnvelope>(body)
        .ok()
        .map(|value| value.error.message)
        .or_else(|| {
            let text = String::from_utf8_lossy(body).trim().to_string();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| "empty response".to_string());
    anyhow!("factoryd returned {status}: {detail}")
}

#[derive(Debug, serde::Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, serde::Deserialize)]
struct ErrorBody {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    #[tokio::test]
    async fn workspace_request_is_not_bound_by_general_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            thread::sleep(Duration::from_millis(80));
            let body = r#"{"jobId":"job-1","repositoryId":"local:test","repository":"repo","baseRef":"HEAD","baseRevision":"abc","branchName":"factory/job-1","root":"/workspace","revision":"abc","state":"active","createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let client = FactorydClient::with_general_timeout(
            &format!("http://{address}"),
            Duration::from_millis(20),
        )
        .unwrap();

        let workspace = client
            .ensure_workspace(
                &JobId::new("job-1"),
                &EnsureWorkspaceRequest {
                    repository_id: "local:test".to_string(),
                    repository: "repo".to_string(),
                    base_ref: "HEAD".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(workspace.job_id, JobId::new("job-1"));
        server.join().unwrap();
    }
}

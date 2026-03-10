use std::time::Duration;

use serde::Deserialize;

pub const GITHUB_COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub const GITHUB_COPILOT_SCOPE: &str = "read:user";
pub const GITHUB_COPILOT_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const GITHUB_COPILOT_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const GITHUB_COPILOT_MODELS_URL: &str = "https://api.githubcopilot.com/models";
pub const GITHUB_COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
pub const GITHUB_COPILOT_EDITOR_VERSION: &str = "vscode/1.99.3";
pub const GITHUB_COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
pub const GITHUB_COPILOT_INTEGRATION_ID: &str = "vscode-chat";

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_poll_interval_secs")]
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GithubCopilotAuthError {
    #[error("failed to start device login: {0}")]
    DeviceCodeRequest(String),
    #[error("failed to poll device login: {0}")]
    TokenPolling(String),
    #[error("device login was denied")]
    AccessDenied,
    #[error("device login expired before authorization completed")]
    Expired,
    #[error("github copilot token validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePollingStatus {
    Pending,
    SlowDown,
    Authorized(String),
}

pub fn default_headers() -> Vec<(String, String)> {
    vec![
        (
            "User-Agent".to_string(),
            GITHUB_COPILOT_USER_AGENT.to_string(),
        ),
        (
            "Editor-Version".to_string(),
            GITHUB_COPILOT_EDITOR_VERSION.to_string(),
        ),
        (
            "Editor-Plugin-Version".to_string(),
            GITHUB_COPILOT_EDITOR_PLUGIN_VERSION.to_string(),
        ),
        (
            "Copilot-Integration-Id".to_string(),
            GITHUB_COPILOT_INTEGRATION_ID.to_string(),
        ),
    ]
}

pub fn default_poll_interval_secs() -> u64 {
    5
}

pub async fn request_device_code(
    client: &reqwest::Client,
) -> Result<DeviceCodeResponse, GithubCopilotAuthError> {
    let response = client
        .post(GITHUB_COPILOT_DEVICE_CODE_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, GITHUB_COPILOT_USER_AGENT)
        .form(&[
            ("client_id", GITHUB_COPILOT_CLIENT_ID),
            ("scope", GITHUB_COPILOT_SCOPE),
        ])
        .send()
        .await
        .map_err(|e| GithubCopilotAuthError::DeviceCodeRequest(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(GithubCopilotAuthError::DeviceCodeRequest(format!(
            "HTTP {status}: {}",
            truncate_for_error(&body)
        )));
    }

    response
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|e| GithubCopilotAuthError::DeviceCodeRequest(e.to_string()))
}

pub async fn poll_for_access_token(
    client: &reqwest::Client,
    device_code: &str,
) -> Result<DevicePollingStatus, GithubCopilotAuthError> {
    let response = client
        .post(GITHUB_COPILOT_ACCESS_TOKEN_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, GITHUB_COPILOT_USER_AGENT)
        .form(&[
            ("client_id", GITHUB_COPILOT_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|e| GithubCopilotAuthError::TokenPolling(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(GithubCopilotAuthError::TokenPolling(format!(
            "HTTP {status}: {}",
            truncate_for_error(&body)
        )));
    }

    let body = response
        .json::<AccessTokenResponse>()
        .await
        .map_err(|e| GithubCopilotAuthError::TokenPolling(e.to_string()))?;

    if let Some(token) = body.access_token {
        return Ok(DevicePollingStatus::Authorized(token));
    }

    match body.error.as_deref() {
        Some("authorization_pending") | None => Ok(DevicePollingStatus::Pending),
        Some("slow_down") => Ok(DevicePollingStatus::SlowDown),
        Some("access_denied") => Err(GithubCopilotAuthError::AccessDenied),
        Some("expired_token") => Err(GithubCopilotAuthError::Expired),
        Some(other) => Err(GithubCopilotAuthError::TokenPolling(
            body.error_description
                .filter(|description| !description.is_empty())
                .unwrap_or_else(|| other.to_string()),
        )),
    }
}

pub async fn wait_for_device_login(
    client: &reqwest::Client,
    device: &DeviceCodeResponse,
) -> Result<String, GithubCopilotAuthError> {
    let expires_at = std::time::Instant::now()
        .checked_add(Duration::from_secs(device.expires_in))
        .ok_or(GithubCopilotAuthError::Expired)?;
    let mut poll_interval = device.interval.max(1);

    loop {
        if std::time::Instant::now() >= expires_at {
            return Err(GithubCopilotAuthError::Expired);
        }

        tokio::time::sleep(Duration::from_secs(poll_interval)).await;

        match poll_for_access_token(client, &device.device_code).await? {
            DevicePollingStatus::Pending => {}
            DevicePollingStatus::SlowDown => {
                poll_interval = poll_interval.saturating_add(5);
            }
            DevicePollingStatus::Authorized(token) => return Ok(token),
        }
    }
}

pub async fn validate_token(
    client: &reqwest::Client,
    token: &str,
) -> Result<(), GithubCopilotAuthError> {
    let mut request = client
        .get(GITHUB_COPILOT_MODELS_URL)
        .bearer_auth(token)
        .timeout(Duration::from_secs(15));

    for (key, value) in default_headers() {
        request = request.header(&key, value);
    }

    let response = request
        .send()
        .await
        .map_err(|e| GithubCopilotAuthError::Validation(e.to_string()))?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(GithubCopilotAuthError::Validation(format!(
        "HTTP {status}: {}",
        truncate_for_error(&body)
    )))
}

fn truncate_for_error(body: &str) -> String {
    const LIMIT: usize = 200;
    if body.len() <= LIMIT {
        return body.to_string();
    }

    let mut end = LIMIT;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &body[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_headers_include_required_identity_headers() {
        let headers = default_headers();
        assert!(headers.iter().any(|(key, value)| {
            key == "Copilot-Integration-Id" && value == GITHUB_COPILOT_INTEGRATION_ID
        }));
        assert!(
            headers
                .iter()
                .any(|(key, value)| key == "Editor-Version"
                    && value == GITHUB_COPILOT_EDITOR_VERSION)
        );
        assert!(
            headers
                .iter()
                .any(|(key, value)| key == "User-Agent" && value == GITHUB_COPILOT_USER_AGENT)
        );
    }

    #[test]
    fn truncate_for_error_preserves_utf8_boundaries() {
        let long = "日本語".repeat(100);
        let truncated = truncate_for_error(&long);
        assert!(truncated.ends_with("..."));
        assert!(truncated.is_char_boundary(truncated.len() - 3));
    }
}

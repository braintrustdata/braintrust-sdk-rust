//! HTTP client for communicating with the Braintrust API

use crate::error::{BraintrustError, Result};
use crate::types::SpanEvent;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default API URL for Braintrust
pub const DEFAULT_API_URL: &str = "https://api.braintrustdata.com";

/// Maximum number of retry attempts
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (in milliseconds)
const BASE_DELAY_MS: u64 = 100;

/// Request body for the logs endpoint
#[derive(Debug, Serialize)]
struct LogsRequest {
    rows: Vec<SpanEvent>,
    api_version: u32,
}

/// Request body for project registration
#[derive(Debug, Serialize)]
struct ProjectRegisterRequest {
    project_name: String,
    org_id: String,
}

/// Response from project registration
#[derive(Debug, serde::Deserialize)]
struct ProjectRegisterResponse {
    project: ProjectInfo,
}

/// Project information
#[derive(Debug, Deserialize)]
struct ProjectInfo {
    id: String,
    name: String,
}

/// Response from API key login
#[derive(Debug, Deserialize)]
struct ApiKeyLoginResponse {
    org_info: Vec<OrgInfo>,
}

/// Organization information
#[derive(Debug, Deserialize)]
struct OrgInfo {
    id: String,
    name: String,
}

/// HTTP client for Braintrust API
#[derive(Debug, Clone)]
pub struct HttpClient {
    client: Client,
    api_key: String,
    base_url: String,
}

impl HttpClient {
    /// Create a new HTTP client
    ///
    /// # Arguments
    ///
    /// * `api_key` - Braintrust API key for authentication
    /// * `base_url` - Optional custom API URL (defaults to production)
    pub fn new(api_key: String, base_url: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        let api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            return Err(BraintrustError::AuthError(
                "API key cannot be empty".to_string(),
            ));
        }

        Ok(HttpClient {
            client,
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        })
    }

    /// Get organization ID from API key
    ///
    /// # Returns
    ///
    /// The organization ID
    pub async fn get_org_id(&self) -> Result<String> {
        let url = format!("{}/api/apikey/login", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(BraintrustError::ApiError {
                status: status.as_u16(),
                message,
            });
        }

        let response: ApiKeyLoginResponse = response.json().await?;

        // Get the first organization (most users have only one)
        response
            .org_info
            .first()
            .map(|org| org.id.clone())
            .ok_or_else(|| {
                BraintrustError::ConfigError("No organization found for this API key".to_string())
            })
    }

    /// Register or get a project by name
    ///
    /// # Arguments
    ///
    /// * `project_name` - Name of the project
    /// * `org_id` - Organization ID
    ///
    /// # Returns
    ///
    /// The project ID
    pub async fn register_project(&self, project_name: String, org_id: String) -> Result<String> {
        let request = ProjectRegisterRequest {
            project_name,
            org_id,
        };

        let url = format!("{}/api/project/register", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(BraintrustError::ApiError {
                status: status.as_u16(),
                message,
            });
        }

        let response: ProjectRegisterResponse = response.json().await?;
        Ok(response.project.id)
    }

    /// Send a batch of span events to the Braintrust API
    ///
    /// # Arguments
    ///
    /// * `events` - Vector of span events to send
    ///
    /// # Returns
    ///
    /// Result indicating success or failure
    pub async fn send_batch(&self, events: Vec<SpanEvent>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let request = LogsRequest {
            rows: events,
            api_version: 2,
        };

        self.send_with_retry("/logs3", &request).await
    }

    /// Send a request with retry logic
    async fn send_with_retry<T: Serialize>(&self, endpoint: &str, body: &T) -> Result<()> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            match self.send_request(&url, body).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_error = Some(e);

                    // Don't retry on client errors (4xx except 429)
                    if let Some(BraintrustError::ApiError { status, .. }) = &last_error {
                        if *status >= 400 && *status < 500 && *status != 429 {
                            break;
                        }
                    }

                    // Calculate exponential backoff delay
                    if attempt < MAX_RETRIES - 1 {
                        let delay = Duration::from_millis(BASE_DELAY_MS * 2_u64.pow(attempt));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            BraintrustError::Other("Request failed after retries".to_string())
        }))
    }

    /// Send a single HTTP request
    async fn send_request<T: Serialize>(&self, url: &str, body: &T) -> Result<()> {
        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        let status = response.status();

        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(BraintrustError::ApiError {
                status: status.as_u16(),
                message,
            });
        }

        Ok(())
    }

    /// Test the connection to the API
    pub async fn ping(&self) -> Result<()> {
        let url = format!("{}/ping", self.base_url);
        let response = self.client.get(&url).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(BraintrustError::ApiError {
                status: response.status().as_u16(),
                message: "Ping failed".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = HttpClient::new("test-key".to_string(), None);
        assert!(client.is_ok());
    }

    #[test]
    fn test_empty_api_key() {
        let client = HttpClient::new("".to_string(), None);
        assert!(client.is_err());
    }

    #[test]
    fn test_custom_base_url() {
        let custom_url = "https://custom.example.com";
        let client = HttpClient::new("test-key".to_string(), Some(custom_url.to_string()));
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.base_url, custom_url);
    }
}

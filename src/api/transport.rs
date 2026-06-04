use std::path::Path;
use std::time::Duration;

use reqwest::{Client, Response};
use serde::de::DeserializeOwned;

use crate::api::logs3::Logs3OverflowUpload;
use crate::error::{BraintrustError, Result};

pub(crate) fn build_http_client(timeout: Duration, ca_bundle: Option<&Path>) -> Result<Client> {
    let mut builder = Client::builder().timeout(timeout);

    if let Some(ca_bundle) = ca_bundle {
        let pem = std::fs::read(ca_bundle).map_err(|err| {
            BraintrustError::InvalidConfig(format!(
                "failed to read CA bundle {}: {}",
                ca_bundle.display(),
                err
            ))
        })?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|err| {
            BraintrustError::InvalidConfig(format!(
                "failed to parse PEM certificates from CA bundle {}: {}",
                ca_bundle.display(),
                err
            ))
        })?;
        if certs.is_empty() {
            return Err(BraintrustError::InvalidConfig(format!(
                "CA bundle {} did not contain any PEM certificates",
                ca_bundle.display()
            )));
        }
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }

    builder
        .build()
        .map_err(|err| BraintrustError::InvalidConfig(err.to_string()))
}

pub(crate) async fn ensure_success_response(response: Response) -> Result<()> {
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(BraintrustError::Api {
            status: status.as_u16(),
            message,
        });
    }

    Ok(())
}

pub(crate) async fn decode_json_response<T>(
    response: Response,
    parse_context: &'static str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(BraintrustError::Api {
            status: status.as_u16(),
            message,
        });
    }

    response
        .json::<T>()
        .await
        .map_err(|err| BraintrustError::Api {
            status: status.as_u16(),
            message: format!("Failed to parse {parse_context}: {err}"),
        })
}

pub(crate) async fn upload_signed_payload(
    client: &Client,
    upload: &Logs3OverflowUpload,
    payload: &[u8],
) -> Result<()> {
    let response = if upload.method().eq_ignore_ascii_case("POST") {
        let fields = upload.fields().ok_or_else(|| {
            BraintrustError::InvalidConfig("overflow POST upload missing fields".into())
        })?;

        let mut form = reqwest::multipart::Form::new();
        for (key, value) in fields {
            form = form.text(key.clone(), value.clone());
        }

        let content_type = fields
            .get("Content-Type")
            .map(|value| value.as_str())
            .unwrap_or("application/json");
        let part = reqwest::multipart::Part::bytes(payload.to_vec())
            .mime_str(content_type)
            .map_err(|err| {
                BraintrustError::InvalidConfig(format!(
                    "invalid overflow content-type '{content_type}': {err}"
                ))
            })?;
        form = form.part("file", part);

        let mut request = client.post(upload.signed_url()).multipart(form);
        if let Some(headers) = upload.headers() {
            for (key, value) in headers {
                if !key.eq_ignore_ascii_case("content-type") {
                    request = request.header(key, value);
                }
            }
        }
        request.send().await
    } else {
        let mut request = client
            .put(upload.signed_url())
            .header("content-type", "application/json")
            .body(payload.to_vec());
        if let Some(headers) = upload.headers() {
            for (key, value) in headers {
                request = request.header(key, value);
            }
        }
        // Azure Blob Storage requires `x-ms-blob-type: BlockBlob` on SAS-URL PUT
        // requests; without it the server returns 400. The TypeScript SDK uses the
        // same hostname-based check (`addAzureBlobHeaders`).
        if upload.signed_url().contains("blob.core.windows.net") {
            request = request.header("x-ms-blob-type", "BlockBlob");
        }
        request.send().await
    }
    .map_err(|err| BraintrustError::Network(err.to_string()))?;

    ensure_success_response(response).await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    pub(crate) const VALID_TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIICpDCCAYwCCQDtlc4RX+IuODANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAls
b2NhbGhvc3QwHhcNMjYwMzE3MTY1MzAyWhcNMjYwMzE4MTY1MzAyWjAUMRIwEAYD
VQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDX
K/y/7AlhzPBkIbiEiCt/l1Qfa99h8FdblOe8BCeJkpoW4Fw10mZnWBgX6peZMF7j
p4rjtIJTWkfl8eNoTPdOkYfmi6B3AwAZzl7VQCMgE0gCFkIrgXrkeLqP+q231UxE
wKgilRG3DWFfELZCQeFtq0jSBcnyWybw+o9SgajaQ+SJg7lbgT6o+8AwHQ54HBo+
VVZJ2CybZvmijQXGiVCMpZ34nxJVW/i6AbsFwp+CLMHOFjrpLuZpv61EnZaGsqsF
RG/VPiNca769Dr8YG4RtPRBKvyDMnUqEDkGwYXrhAVxvI3kKlQq3MHppGCSsjnVl
oqhWm//sE7znMJtuzIf7AgMBAAEwDQYJKoZIhvcNAQELBQADggEBAD1zS7eOkfU2
IzxjW7MAJce5JrAcRWWe3L2ORx+y+PS4uI0ms1FM4AopZ2FxXdbSSXLf5bqC2f2i
qy+8YbVdZacFtFLmnZicCXP86Na5JUYxZERDyqKN4GFwSrfELwLsuv9TWpir+p/H
3XxQ/8/eJdTHOunNtl4BVUefjGp9PVNb6NFvLDkSkNN37KcjNpB9jPVK970uZ5lb
kOx6ulbMXpNH73h5rwzgs6FbVbcAavPJKYGr170rDRxidpfRz3ex+RBQvcfFQeRx
NP64Q8OosOHraKRn7bvST7bXvGFZUp06aIFrlwdmSQPXU/6o4zYNmkR4RVv4VvQ7
cb0bfZ7fHHs=
-----END CERTIFICATE-----
"#;

    pub(crate) fn write_temp_bundle(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "braintrust-sdk-rust-ca-bundle-{}.pem",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).expect("write temp bundle");
        path
    }

    #[test]
    fn rejects_missing_bundle_path() {
        let path = std::env::temp_dir().join(format!(
            "braintrust-sdk-rust-missing-{}.pem",
            uuid::Uuid::new_v4()
        ));

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to read CA bundle"));
    }

    #[test]
    fn rejects_empty_bundle() {
        let path = write_temp_bundle("");

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err
            .to_string()
            .contains("did not contain any PEM certificates"));
    }

    #[test]
    fn rejects_malformed_bundle() {
        let path = write_temp_bundle(
            "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
        );

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to parse PEM certificates"));
    }

    #[test]
    fn accepts_valid_bundle() {
        let path = write_temp_bundle(VALID_TEST_CERT_PEM);

        let client = build_http_client(Duration::from_secs(1), Some(&path));
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(client.is_ok());
    }
}

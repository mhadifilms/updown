use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::info;

/// S3-compatible storage backend.
///
/// Supports AWS S3, Cloudflare R2, MinIO, and any S3-compatible service.
/// This is what makes updown a Faspex replacement: fast UDP transfer
/// between a storage bucket and a user.
///
/// Architecture:
///   S3 bucket → updown server (pulls objects) → UDP blast → updown client
///   updown client → UDP blast → updown server → S3 bucket (pushes objects)
pub struct S3Backend {
    client: Client,
    bucket: String,
}

impl S3Backend {
    /// Create from AWS default config (reads AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_REGION)
    pub async fn from_env(bucket: &str) -> Result<Self> {
        let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
        let client = Client::new(&config);
        Ok(Self {
            client,
            bucket: bucket.to_string(),
        })
    }

    /// Create for Cloudflare R2 or custom S3-compatible endpoint
    pub async fn from_endpoint(
        bucket: &str,
        endpoint_url: &str,
        access_key: &str,
        secret_key: &str,
        region: &str,
    ) -> Result<Self> {
        let creds = aws_credential_types::Credentials::new(
            access_key,
            secret_key,
            None,
            None,
            "updown",
        );
        let config = aws_config::defaults(BehaviorVersion::latest())
            .endpoint_url(endpoint_url)
            .region(aws_config::Region::new(region.to_string()))
            .credentials_provider(creds)
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true) // Required for MinIO
            .build();
        let client = Client::from_conf(s3_config);
        Ok(Self {
            client,
            bucket: bucket.to_string(),
        })
    }

    /// Download an S3 object to a local file for transfer.
    /// Returns the local file path.
    pub async fn download_to_local(
        &self,
        key: &str,
        local_dir: &Path,
    ) -> Result<PathBuf> {
        let filename = key.rsplit('/').next().unwrap_or(key);
        let local_path = local_dir.join(filename);

        info!("Downloading s3://{}/{} -> {}", self.bucket, key, local_path.display());

        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .context("S3 GetObject failed")?;

        let mut file = File::create(&local_path).await?;
        let mut stream = resp.body.into_async_read();
        tokio::io::copy(&mut stream, &mut file).await?;
        file.flush().await?;

        let metadata = tokio::fs::metadata(&local_path).await?;
        info!(
            "Downloaded {} bytes from s3://{}/{}",
            metadata.len(),
            self.bucket,
            key
        );

        Ok(local_path)
    }

    /// Upload a local file to S3.
    pub async fn upload_from_local(
        &self,
        local_path: &Path,
        key: &str,
    ) -> Result<()> {
        let body = aws_sdk_s3::primitives::ByteStream::from_path(local_path)
            .await
            .context("failed to read file for S3 upload")?;

        let metadata = tokio::fs::metadata(local_path).await?;
        info!(
            "Uploading {} ({} bytes) -> s3://{}/{}",
            local_path.display(),
            metadata.len(),
            self.bucket,
            key
        );

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(body)
            .send()
            .await
            .context("S3 PutObject failed")?;

        info!("Uploaded to s3://{}/{}", self.bucket, key);
        Ok(())
    }

    /// List objects in a prefix (like a directory listing).
    pub async fn list_objects(&self, prefix: &str) -> Result<Vec<S3Object>> {
        let mut objects = Vec::new();
        let mut continuation_token = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.context("S3 ListObjects failed")?;

            if let Some(contents) = resp.contents {
                for obj in contents {
                    objects.push(S3Object {
                        key: obj.key.unwrap_or_default(),
                        size: obj.size.unwrap_or(0) as u64,
                    });
                }
            }

            if resp.is_truncated.unwrap_or(false) {
                continuation_token = resp.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(objects)
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }
}

#[derive(Debug, Clone)]
pub struct S3Object {
    pub key: String,
    pub size: u64,
}

use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};

use anyhow::Result;
use futures::StreamExt;
use kemono_api::API;
use once_cell::sync::Lazy;
use regex::Regex;

use tokio::{fs::File, io::AsyncWriteExt};
use tracing::{error, info, info_span, warn};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::DONE;

/// 提取 web_name 和 user_id
pub fn extract_info(url: &str) -> (Option<String>, Option<String>) {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"https://[^/]+/([^/]+)/user/(\d+)").unwrap());
    if let Some(caps) = RE.captures(url) {
        let web_name = caps.get(1).map(|m| m.as_str().to_string());
        let user_id = caps.get(2).map(|m| m.as_str().to_string());
        (web_name, user_id)
    } else {
        (None, None)
    }
}

pub async fn download_file(
    api: Arc<API>,
    url: &str,
    save_dir: &Path,
    file_name: &str,
) -> Result<()> {
    if DONE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let save_path = save_dir.join(file_name);

    let head_resp = api.head(url).await?;
    if !head_resp.status().is_success() {
        error!(
            "failed to download {} status_code {:?}",
            url,
            head_resp.status()
        );
        return Ok(());
    }
    let total_size = head_resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    if save_path.exists() && save_path.is_file() {
        let metadata = std::fs::metadata(&save_path)?;
        if metadata.len() == total_size && total_size > 0 {
            warn!("File already exists, skipped {}", file_name);
            return Ok(());
        }
    }

    let resp = api.get_stream(url).await?;
    if !resp.status().is_success() {
        error!("failed to download {} status_code {:?}", url, resp.status());
        return Ok(());
    }

    let span = info_span!("download");
    span.pb_set_message(&format!("Downloading {}", file_name));
    span.pb_set_length(total_size);
    let _enter = span.enter();

    let mut file = File::create(&save_path).await?;
    let mut stream = resp.bytes_stream();

    let mut pos = 0;
    while let Some(item) = stream.next().await {
        let data = match item {
            Ok(d) => d,
            Err(e) => {
                // pb.finish_with_message("Error occurred!");
                return Err(e.into());
            }
        };

        if DONE.load(Ordering::Relaxed) {
            drop(file);
            tokio::fs::remove_file(&save_path).await.ok();
            return Ok(());
        }

        file.write_all(&data).await?;
        let len = data.len() as u64;
        pos += len;
        span.pb_set_position(pos);
    }
    file.flush().await?;

    // workaround for tracing-indicatif deadlock bu
    // TODO: fix in upstream
    span.pb_finish_clear();
    drop(_enter);
    drop(span);
    info!("Completed {file_name}");
    Ok(())
}

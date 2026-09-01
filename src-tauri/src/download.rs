use crate::domain::Track;
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

fn safe_filename(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "track".into()
    } else {
        trimmed.chars().take(120).collect()
    }
}

pub async fn download_track(
    client: &reqwest::Client,
    track: &Track,
    directory: &Path,
) -> Result<PathBuf, String> {
    if track.audio_url.is_empty() {
        return Err("此音源没有提供可下载地址".into());
    }
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| format!("无法创建下载目录: {error}"))?;
    let extension = track
        .adapter_payload
        .as_ref()
        .and_then(|payload| payload.get("extension"))
        .and_then(|value| value.as_str())
        .unwrap_or("mp3");
    let filename = format!(
        "{} - {}.{}",
        safe_filename(&track.title),
        safe_filename(&track.artist),
        safe_filename(extension)
    );
    let path = directory.join(filename);
    let mut request = client.get(&track.audio_url);
    if let Some(headers) = track
        .adapter_payload
        .as_ref()
        .and_then(|payload| payload.get("downloadHeaders"))
        .and_then(|value| value.as_object())
    {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("下载请求失败: {error}"))?
        .error_for_status()
        .map_err(|error| format!("下载地址返回错误: {error}"))?;
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|error| format!("无法创建下载文件: {error}"))?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk.map_err(|error| format!("读取下载流失败: {error}"))?)
            .await
            .map_err(|error| format!("写入下载文件失败: {error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("保存下载文件失败: {error}"))?;
    Ok(path)
}

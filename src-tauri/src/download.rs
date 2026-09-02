use crate::domain::Track;
use futures_util::StreamExt;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, Tag};
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
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<PathBuf, String> {
    if track.audio_url.is_empty() {
        return Err("此音源没有提供可下载地址".into());
    }
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| format!("无法创建下载目录: {error}"))?;
    // Prefer the source-reported container, then the resolved `format` (e.g.
    // `FLAC`), so a lossless download is never mislabelled `mp3`.
    let extension = track
        .adapter_payload
        .as_ref()
        .and_then(|payload| payload.get("extension"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_ascii_lowercase())
        .or_else(|| {
            track
                .format
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.to_ascii_lowercase())
                .filter(|value| value.len() <= 5)
        })
        .unwrap_or_else(|| "mp3".into());
    let filename = format!(
        "{} - {}.{}",
        safe_filename(&track.title),
        safe_filename(&track.artist),
        safe_filename(&extension)
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
    while let Some(chunk) =
        tokio::select! {
            biased;
            _ = &mut cancel => return Err("已停止下载".into()),
            chunk = stream.next() => chunk,
        }
    {
        // Re-check the cancel signal so a large final chunk is still interruptible.
        if cancel.is_terminated() {
            return Err("已停止下载".into());
        }
        file.write_all(&chunk.map_err(|error| format!("读取下载流失败: {error}"))?)
            .await
            .map_err(|error| format!("写入下载文件失败: {error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("保存下载文件失败: {error}"))?;
    Ok(path)
}

const ARTWORK_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";

/// Best-effort download of the track's artwork bytes, or `None` when there is
/// no artwork or it cannot be fetched.
async fn fetch_cover(client: &reqwest::Client, track: &Track) -> Option<Vec<u8>> {
    let url = track.artwork_url.trim();
    if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let mut request = client.get(url).header(reqwest::header::USER_AGENT, ARTWORK_UA);
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
    let response = request.send().await.ok()?.error_for_status().ok()?;
    let bytes = response.bytes().await.ok()?;
    if bytes.len() < 16 {
        return None;
    }
    Some(bytes.to_vec())
}

/// Write the track's title / artist / album tags and (when available) album
/// cover into the downloaded audio file, via `lofty`.
///
/// Best-effort: a missing cover or unprintable metadata never fails the download.
pub async fn write_metadata(
    client: &reqwest::Client,
    track: &Track,
    path: &Path,
) -> Result<(), String> {
    let cover = fetch_cover(client, track).await;
    let path = path.to_path_buf();
    let title = track.title.clone();
    let artist = track.artist.clone();
    let album = track.album.clone();
    tauri::async_runtime::spawn_blocking(move || {
        write_metadata_blocking(&path, &title, &artist, &album, cover.as_deref())
    })
    .await
    .map_err(|error| format!("元数据写入任务中断: {error}"))??;
    Ok(())
}

fn write_metadata_blocking(
    path: &Path,
    title: &str,
    artist: &str,
    album: &str,
    cover: Option<&[u8]>,
) -> Result<(), String> {
    let mut file = lofty::read_from_path(path)
        .map_err(|error| format!("无法读取音频元数据: {error}"))?;
    let tag_type = file.file_type().primary_tag_type();
    let picture = cover.and_then(|data| {
        crate::sniff_image_type(data)
            .map(|mime| (data.to_vec(), MimeType::from_str(mime)))
            .map(|(bytes, mime)| {
                Picture::unchecked(bytes)
                    .pic_type(PictureType::CoverFront)
                    .mime_type(mime)
                    .build()
            })
    });

    {
        let tag = if let Some(tag) = file.primary_tag_mut() {
            tag
        } else if let Some(tag) = file.first_tag_mut() {
            tag
        } else {
            file.insert_tag(Tag::new(tag_type));
            file.primary_tag_mut()
                .expect("freshly inserted primary tag should be present")
        };
        if !title.trim().is_empty() {
            tag.set_title(title.trim().to_owned());
        }
        if !artist.trim().is_empty() {
            tag.set_artist(artist.trim().to_owned());
        }
        if !album.trim().is_empty() {
            tag.set_album(album.trim().to_owned());
        }
        if let Some(picture) = picture {
            tag.remove_picture_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
    }

    file.save_to_path(path, WriteOptions::default())
        .map_err(|error| format!("无法写入元数据: {error}"))
}


#[cfg(test)]
mod tests {
    use super::write_metadata_blocking;
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;
    use std::fs;

    fn minimal_wav() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&0u32.to_le_bytes()); // size patched below
        data.extend_from_slice(b"WAVE");
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        data.extend_from_slice(&1u16.to_le_bytes()); // PCM
        data.extend_from_slice(&1u16.to_le_bytes()); // channels
        data.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        data.extend_from_slice(&8000u32.to_le_bytes()); // byte rate
        data.extend_from_slice(&1u16.to_le_bytes()); // block align
        data.extend_from_slice(&8u16.to_le_bytes()); // bits per sample
        data.extend_from_slice(b"data");
        data.extend_from_slice(&4u32.to_le_bytes()); // data size
        data.extend_from_slice(&[0, 0, 0, 0]);
        let total = data.len() as u32;
        data[4..8].copy_from_slice(&(total - 8).to_le_bytes());
        data
    }

    #[test]
    fn writes_title_artist_album() {
        let path = std::env::temp_dir().join(format!("litemusicdl-md-test-{}.wav", std::process::id()));
        fs::write(&path, minimal_wav()).unwrap();
        let result = write_metadata_blocking(&path, "标题", "歌手", "专辑", None);
        assert!(result.is_ok(), "metadata write failed: {result:?}");
        let file = lofty::read_from_path(&path).expect("should read back the written file");
        let tag = file.primary_tag().expect("should have a primary tag");
        assert_eq!(tag.title().as_deref(), Some("标题"));
        assert_eq!(tag.artist().as_deref(), Some("歌手"));
        assert_eq!(tag.album().as_deref(), Some("专辑"));
        fs::remove_file(path).unwrap();
    }
}

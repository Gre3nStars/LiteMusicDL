mod domain;
mod download;
mod sources;

use crate::domain::{SourceDescriptor, Track};
use futures_util::future::join_all;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::Accessor;
use serde_json::{json, Value};
use sources::registry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

struct AppState {
    client: reqwest::Client,
}

struct PlaybackState {
    tracks: Mutex<HashMap<String, Track>>,
    next_id: AtomicU64,
}

struct DownloadState {
    cancels: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
}

#[derive(Clone)]
struct LocalArtwork {
    content_type: String,
    bytes: Vec<u8>,
}

struct LocalArtworkState {
    artwork: Mutex<HashMap<String, LocalArtwork>>,
    next_id: AtomicU64,
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("LiteMusicDL/0.1")
        .connect_timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()
        .expect("failed to build LiteMusicDL HTTP client")
}

fn media_content_type(track: &Track, headers: &reqwest::header::HeaderMap) -> String {
    let received = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let normalized = received
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = track
        .format
        .as_deref()
        .or_else(|| {
            track
                .audio_url
                .split('?')
                .next()
                .and_then(|url| url.rsplit('.').next())
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    // The type implied by the resolved container/extension.
    let type_from_ext = match extension.as_str() {
        "flac" => Some("audio/flac"),
        "m4a" | "mp4" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "aif" | "aiff" => Some("audio/aiff"),
        "mp3" | "mpeg" | "mp2" => Some("audio/mpeg"),
        _ => None,
    };
    match normalized.as_str() {
        // Kuwo's active fallback returns `audio/x-flac`. WebKit's custom URI
        // media path recognises the standard form reliably, while it may reject
        // the legacy x- prefix with MEDIA_ERR_SRC_NOT_SUPPORTED (error 4).
        "audio/x-flac" | "application/flac" | "application/x-flac" => "audio/flac".into(),
        "audio/x-wav" | "audio/wave" => "audio/wav".into(),
        "audio/x-m4a" | "audio/mp4" | "video/mp4" => "audio/mp4".into(),
        // Some sources mislabel the container: QQ tang returns FLAC bytes but
        // `audio/x-ogg`, and Netease lossless returns FLAC as `audio/mpeg`.
        // Trust the real container when the header contradicts it.
        "audio/x-ogg" | "audio/ogg"
            if type_from_ext.is_some()
                && !matches!(extension.as_str(), "ogg" | "oga" | "opus") =>
        {
            type_from_ext.unwrap().into()
        }
        "audio/x-ogg" => "audio/ogg".into(),
        "audio/mpeg" | "audio/x-mpeg"
            if type_from_ext.is_some()
                && !matches!(extension.as_str(), "mp3" | "mpeg" | "mp2") =>
        {
            type_from_ext.unwrap().into()
        }
        _ if normalized.starts_with("audio/") => normalized,
        _ => type_from_ext.unwrap_or("audio/mpeg").into(),
    }
}

fn is_audio_path(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some(
            "mp3"
                | "flac"
                | "m4a"
                | "aac"
                | "ogg"
                | "oga"
                | "opus"
                | "wav"
                | "aif"
                | "aiff"
                | "alac"
        )
    )
}

/// True when the file's leading bytes look like a real audio container/frame.
/// Used to reject playlist/lyric/text files that were renamed with an audio
/// extension and that `lofty` (correctly) refused to parse.
fn has_audio_magic(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut sample = [0u8; 64];
    let Ok(read) = file.read(&mut sample) else {
        return false;
    };
    let sample = &sample[..read];
    if sample.is_empty() {
        return false;
    }
    sample.starts_with(b"ID3")
        || (sample[0] == 0xff && sample.get(1).is_some_and(|byte| byte & 0xe0 == 0xe0))
        || sample.starts_with(b"fLaC")
        || sample.starts_with(b"OggS")
        || sample.starts_with(b"RIFF")
        || sample.starts_with(b"FORM")
        || sample.starts_with(b".snd")
        || sample.get(4..8) == Some(b"ftyp")
}

fn is_text_like(bytes: &[u8]) -> bool {
    // UTF-16 BOM -> almost certainly a text/lyric file.
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        return true;
    }
    // Skip a UTF-8 BOM before the real content sniff.
    let sample = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        &bytes[3..]
    } else {
        bytes
    };
    let sample = &sample[..sample.len().min(4_096)];
    // A lyric/text file begins with a bracket tag such as "[00:01.00]" or
    // "[ti:...]". Real audio starts with an ID3 frame, a sync word, or a
    // container magic, so this is a safe discriminator.
    sample.starts_with(b"[") && sample[..sample.len().min(256)].contains(&b']')
}

fn is_lyric_file(path: &std::path::Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    // Non-audio companion/playlist/lyric files are never songs.
    if matches!(
        extension.as_deref(),
        Some("lrc" | "txt" | "cue" | "m3u" | "m3u8" | "pls" | "nfo" | "log" | "srt")
    ) {
        return true;
    }
    let Ok(contents) = std::fs::read(path) else {
        return false;
    };
    // A number of LRC files use GBK, so checking UTF-8 validity here would
    // incorrectly let a renamed `*.lrc.mp3` text file into the music list.
    is_text_like(&contents)
}

pub(crate) fn sniff_image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

fn local_artwork_from_bytes(content_type: &str, bytes: Vec<u8>) -> Option<LocalArtwork> {
    if bytes.is_empty() || bytes.len() > 30 * 1024 * 1024 {
        return None;
    }
    // Prefer sniffing the real image type; many tag writers store an empty or
    // non-standard mime (e.g. "image/jpg"), which would otherwise hide covers.
    let content_type = sniff_image_type(&bytes).or_else(|| match content_type {
        "image/jpg" => Some("image/jpeg"),
        "image/jpeg" | "image/png" | "image/gif" | "image/bmp" | "image/webp" => Some(content_type),
        _ => None,
    })?;
    Some(LocalArtwork {
        content_type: content_type.into(),
        bytes,
    })
}

fn local_artwork(tagged_file: &lofty::file::TaggedFile) -> Option<LocalArtwork> {
    let picture = tagged_file
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| {
            tagged_file
                .tags()
                .iter()
                .flat_map(|tag| tag.pictures())
                .next()
        })?;
    // Pass a possibly-empty/odd mime; byte sniffing will recover the type.
    let content_type = picture
        .mime_type()
        .map(|mime| mime.as_str().to_string())
        .unwrap_or_default();
    local_artwork_from_bytes(&content_type, picture.data().to_vec())
}

fn sidecar_artwork(path: &std::path::Path) -> Option<LocalArtwork> {
    let directory = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy();
    let mut candidates = ["jpg", "jpeg", "png", "webp"]
        .into_iter()
        .map(|extension| directory.join(format!("{stem}.{extension}")))
        .chain(["cover", "folder", "front"].into_iter().flat_map(|name| {
            ["jpg", "jpeg", "png", "webp"]
                .into_iter()
                .map(move |extension| directory.join(format!("{name}.{extension}")))
        }))
        .collect::<Vec<_>>();
    let extra_images = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let candidate = entry.path();
            let is_image = candidate
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "webp"
                    )
                });
            (candidate.is_file() && is_image).then_some(candidate)
        })
        .collect::<Vec<_>>();
    // A lone image in an album folder is an unambiguous cover fallback, even
    // if it does not use a conventional filename such as cover.jpg.
    if extra_images.len() == 1
        && !candidates
            .iter()
            .any(|candidate| candidate == &extra_images[0])
    {
        candidates.push(extra_images[0].clone());
    }
    candidates.into_iter().find_map(|path| {
        let content_type = match path.extension().and_then(|extension| extension.to_str())? {
            extension
                if extension.eq_ignore_ascii_case("jpg")
                    || extension.eq_ignore_ascii_case("jpeg") =>
            {
                "image/jpeg"
            }
            extension if extension.eq_ignore_ascii_case("png") => "image/png",
            extension if extension.eq_ignore_ascii_case("webp") => "image/webp",
            _ => return None,
        };
        std::fs::read(path)
            .ok()
            .and_then(|bytes| local_artwork_from_bytes(content_type, bytes))
    })
}

fn local_track(path: &std::path::Path) -> Option<(Track, Option<LocalArtwork>)> {
    if is_lyric_file(path) {
        return None;
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let filename = path.file_stem()?.to_string_lossy();
    let (mut artist, mut title) = filename
        .split_once(" - ")
        .map(|(artist, title)| (artist.trim().to_owned(), title.trim().to_owned()))
        .unwrap_or_else(|| (String::new(), filename.into_owned()));
    let mut album = String::new();
    let mut duration_ms = 0;
    let mut artwork = None;
    if let Ok(tagged_file) = lofty::read_from_path(path) {
        if let Some(tag) = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())
        {
            if let Some(value) = tag.title().filter(|value| !value.trim().is_empty()) {
                title = value.into_owned();
            }
            if let Some(value) = tag.artist().filter(|value| !value.trim().is_empty()) {
                artist = value.into_owned();
            }
            if let Some(value) = tag.album().filter(|value| !value.trim().is_empty()) {
                album = value.into_owned();
            }
        }
        duration_ms = tagged_file
            .properties()
            .duration()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        artwork = local_artwork(&tagged_file);
    } else if !has_audio_magic(path) {
        // Not parseable as audio and not a real container: this is a text,
        // playlist, or lyric file that was renamed with an audio extension.
        // Never surface it as a song.
        return None;
    }
    artwork = artwork.or_else(|| sidecar_artwork(path));
    let absolute = path.to_string_lossy().into_owned();
    Some((
        Track {
            id: format!("local:{absolute}"),
            source: "LocalMusicClient".into(),
            title,
            artist,
            album,
            artwork_url: String::new(),
            audio_url: absolute.clone(),
            duration_ms,
            format: Some(extension.to_uppercase()),
            quality: None,
            adapter_payload: Some(json!({"localPath": absolute, "extension": extension})),
        },
        artwork,
    ))
}

fn collect_local_tracks(
    directory: &std::path::Path,
    tracks: &mut Vec<(Track, Option<LocalArtwork>)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("无法读取目录 {}: {error}", directory.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let _ = collect_local_tracks(&path, tracks);
        } else if file_type.is_file() && is_audio_path(&path) && !is_lyric_file(&path) {
            if let Some(track) = local_track(&path) {
                tracks.push(track);
            }
        }
    }
    Ok(())
}

fn local_path(track: &Track) -> Option<PathBuf> {
    track
        .adapter_payload
        .as_ref()
        .and_then(|payload| payload.get("localPath"))
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn local_lyrics_path(path: &std::path::Path) -> Option<PathBuf> {
    let direct = path.with_extension("lrc");
    if direct.is_file() {
        return Some(direct);
    }
    let stem = path.file_stem()?;
    let directory = path.parent()?;
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lrc"))
                && candidate
                    .file_stem()
                    .is_some_and(|candidate_stem| candidate_stem == stem)
        })
}

fn decode_local_lyrics(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let values = bytes[2..]
            .chunks_exact(2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&values).map_err(|error| format!("本地歌词编码无效: {error}"));
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        let values = bytes[2..]
            .chunks_exact(2)
            .map(|value| u16::from_be_bytes([value[0], value[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&values).map_err(|error| format!("本地歌词编码无效: {error}"));
    }
    String::from_utf8(bytes).map_err(|error| format!("本地歌词不是 UTF-8 或 UTF-16: {error}"))
}

fn requested_local_range(
    range: Option<&tauri::http::HeaderValue>,
    total: u64,
) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let default = (0, total.saturating_sub(1).min(65_535));
    let Some(raw) = range.and_then(|value| value.to_str().ok()) else {
        return Some(default);
    };
    let Some(raw) = raw.trim().strip_prefix("bytes=") else {
        return Some(default);
    };
    let Some((start, end)) = raw
        .split(',')
        .next()
        .and_then(|value| value.split_once('-'))
    else {
        return Some(default);
    };
    if start.is_empty() {
        let length = end.parse::<u64>().ok()?;
        let length = length.min(total);
        return Some((total - length, total - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= total {
        return None;
    }
    let end = end
        .parse::<u64>()
        .ok()
        .unwrap_or(total - 1)
        .min(total - 1)
        .max(start);
    Some((start, end))
}

#[tauri::command]
async fn list_sources(state: State<'_, AppState>) -> Result<Vec<SourceDescriptor>, String> {
    Ok(registry(state.client.clone())
        .iter()
        .map(|source| source.descriptor())
        .collect())
}

#[tauri::command]
async fn search_tracks(
    state: State<'_, AppState>,
    query: String,
    sources: Vec<String>,
    limit: u32,
    page: u32,
) -> Result<Vec<Track>, String> {
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let selected: HashSet<_> = sources.into_iter().collect();
    let adapters = registry(state.client.clone())
        .into_iter()
        .filter(|source| selected.is_empty() || selected.contains(source.descriptor().id))
        .collect::<Vec<_>>();
    if adapters.is_empty() {
        return Err("没有选中已实现的 Rust 音源适配器".into());
    }

    let tasks = adapters.into_iter().map(|adapter| {
        let query = query.clone();
        async move {
            let descriptor = adapter.descriptor();
            let name = descriptor.name;
            // Transient network errors are common; retry a failed source once
            // before giving up on it (the result is the retry's outcome).
            let result = match adapter.search(&query, limit.max(1) as usize, page.max(1)).await {
                Ok(tracks) => Ok(tracks),
                Err(_) => adapter.search(&query, limit.max(1) as usize, page.max(1)).await,
            };
            (name, result)
        }
    });
    let mut tracks = Vec::new();
    let mut errors = Vec::new();
    for (name, result) in join_all(tasks).await {
        match result {
            Ok(mut source_tracks) => tracks.append(&mut source_tracks),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    if tracks.is_empty() && !errors.is_empty() {
        Err(errors.join("\n"))
    } else {
        Ok(tracks)
    }
}

#[tauri::command]
async fn resolve_qualities(state: State<'_, AppState>, tracks: Vec<Track>) -> Result<Vec<Track>, String> {
    // The search card's quality is metadata-only and can over- or under-report
    // (lossless may be VIP-gated). Resolve each track's *actual* playable tier so
    // the displayed quality matches what playback will use. Locals pass through
    // unchanged; failures keep the metadata quality. Processed in small batches to
    // avoid hammering the sources.
    let registry = registry(state.client.clone());
    let mut tracks = tracks;
    let mut output = Vec::with_capacity(tracks.len());
    while !tracks.is_empty() {
        let batch: Vec<_> = tracks.drain(..tracks.len().min(6)).collect();
        let futures = batch.into_iter().map(|track| {
            let registry = registry.clone();
            async move {
                if local_path(&track).is_some() || !track.audio_url.trim().is_empty() {
                    return track;
                }
                let Some(source) = registry
                    .iter()
                    .find(|source| source.descriptor().id == track.source)
                    .cloned()
                else {
                    return track;
                };
                match source.resolve_quality(&track).await {
                    Some((quality, format)) => Track { quality: Some(quality), format: Some(format), ..track },
                    None => track,
                }
            }
        });
        output.extend(futures_util::future::join_all(futures).await);
    }
    Ok(output)
}

async fn resolve_track_for_action(client: reqwest::Client, track: Track) -> Result<Track, String> {
    if !track.audio_url.trim().is_empty() || local_path(&track).is_some() {
        return Ok(track);
    }
    let source = registry(client)
        .into_iter()
        .find(|source| source.descriptor().id == track.source)
        .ok_or_else(|| format!("未找到 {} 的音源适配器", track.source))?;
    // Retry a transient resolution failure once before surfacing an error to
    // the user, which smooths over congested-network hiccups.
    match source.resolve_track(&track).await {
        Ok(track) => Ok(track),
        Err(first) => match source.resolve_track(&track).await {
            Ok(track) => Ok(track),
            Err(_) => Err(first),
        },
    }
}

#[tauri::command]
async fn scan_local_music(
    artwork_state: State<'_, LocalArtworkState>,
    directory: String,
) -> Result<Vec<Track>, String> {
    let directory = PathBuf::from(directory.trim());
    if !directory.is_dir() {
        return Err("请选择一个可访问的音乐文件夹".into());
    }
    let scanned = tauri::async_runtime::spawn_blocking(move || {
        let mut tracks = Vec::new();
        collect_local_tracks(&directory, &mut tracks)?;
        tracks.sort_by(|left, right| {
            left.0
                .title
                .to_lowercase()
                .cmp(&right.0.title.to_lowercase())
        });
        Ok::<Vec<(Track, Option<LocalArtwork>)>, String>(tracks)
    })
    .await
    .map_err(|error| format!("本地扫描任务中断: {error}"))??;
    let mut artwork = artwork_state
        .artwork
        .lock()
        .map_err(|_| "本地封面缓存不可用".to_string())?;
    artwork.clear();
    Ok(scanned
        .into_iter()
        .map(|(mut track, image)| {
            if let Some(image) = image {
                let token = artwork_state
                    .next_id
                    .fetch_add(1, Ordering::Relaxed)
                    .to_string();
                artwork.insert(token.clone(), image);
                track.artwork_url = format!("localart://localhost/{token}");
            }
            track
        })
        .collect())
}

#[tauri::command]
async fn download_track(
    app: AppHandle,
    state: State<'_, AppState>,
    downloads: State<'_, DownloadState>,
    track: Track,
    directory: Option<String>,
    id: String,
) -> Result<String, String> {
    let track = resolve_track_for_action(state.client.clone(), track).await?;
    let base = match directory.filter(|value| !value.trim().is_empty()) {
        Some(directory) => PathBuf::from(directory),
        None => app
            .path()
            .audio_dir()
            .or_else(|_| app.path().download_dir())
            .map(|path| path.join("LiteMusicDL"))
            .map_err(|error| format!("无法确定系统音乐或下载目录: {error}"))?,
    };
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    if let Ok(mut map) = downloads.cancels.lock() {
        if let Some(previous) = map.insert(id.clone(), cancel_tx) {
            let _ = previous;
        }
    }
    let result = download::download_track(&state.client, &track, &base, cancel_rx).await;
    if let Ok(mut map) = downloads.cancels.lock() {
        map.remove(&id);
    }
    let path = result?;

    // Write the track's title/artist/album tags and, when available, the album
    // cover into the downloaded file. Best-effort: a cover or tag failure never
    // fails an otherwise-successful download.
    if let Err(error) = download::write_metadata(&state.client, &track, &path).await {
        eprintln!("LiteMusicDL: 标签/封面写入失败: {error}");
    }

    if let Some(source) = registry(state.client.clone())
        .into_iter()
        .find(|source| source.descriptor().id == track.source)
    {
        if let Ok(Some(lyrics)) = source.lyrics(&track).await {
            let _ = tokio::fs::write(path.with_extension("lrc"), lyrics).await;
        }
    }
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn cancel_download(downloads: State<'_, DownloadState>, id: String) -> Result<(), String> {
    let sender = downloads
        .cancels
        .lock()
        .map_err(|_| "下载任务状态不可用".to_string())?
        .remove(&id);
    match sender {
        Some(sender) => {
            let _ = sender.send(());
            Ok(())
        }
        None => Err("该下载任务已结束或不存在".into()),
    }
}

/// Delete a downloaded audio file (and its `.lrc` sidecar) from disk.
#[tauri::command]
async fn delete_file(path: String) -> Result<(), String> {
    let path = PathBuf::from(path.trim());
    if !path.is_file() {
        return Err("文件不存在".into());
    }
    tokio::fs::remove_file(&path)
        .await
        .map_err(|error| format!("无法删除文件 {}: {error}", path.display()))?;
    let lyrics = path.with_extension("lrc");
    if lyrics.is_file() {
        let _ = tokio::fs::remove_file(&lyrics).await;
    }
    Ok(())
}

#[tauri::command]
async fn get_lyrics(state: State<'_, AppState>, track: Track) -> Result<Option<String>, String> {
    if let Some(path) = local_path(&track) {
        let Some(lyrics_path) = local_lyrics_path(&path) else {
            return Ok(None);
        };
        return match tokio::fs::read(&lyrics_path).await {
            Ok(bytes) => {
                let lyrics = decode_local_lyrics(bytes)?;
                Ok((!lyrics.trim().is_empty()).then_some(lyrics))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "无法读取本地歌词 {}: {error}",
                lyrics_path.display()
            )),
        };
    }
    let Some(source) = registry(state.client.clone())
        .into_iter()
        .find(|source| source.descriptor().id == track.source)
    else {
        return Ok(None);
    };
    source.lyrics(&track).await
}

fn download_history_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("download-history.json"))
        .map_err(|error| format!("无法确定下载记录目录: {error}"))
}

#[tauri::command]
async fn load_download_history(app: AppHandle) -> Result<Vec<Value>, String> {
    let path = download_history_path(&app)?;
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|error| format!("下载记录文件损坏: {error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("无法读取下载记录: {error}")),
    }
}

#[tauri::command]
async fn save_download_history(app: AppHandle, records: Vec<Value>) -> Result<(), String> {
    let path = download_history_path(&app)?;
    let directory = path
        .parent()
        .ok_or_else(|| "下载记录目录不可用".to_string())?;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| format!("无法创建下载记录目录: {error}"))?;
    let serialized = serde_json::to_vec_pretty(&records)
        .map_err(|error| format!("无法序列化下载记录: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, serialized)
        .await
        .map_err(|error| format!("无法写入下载记录: {error}"))?;
    tokio::fs::rename(temporary, path)
        .await
        .map_err(|error| format!("无法保存下载记录: {error}"))
}

#[tauri::command]
async fn prepare_playback(
    state: State<'_, AppState>,
    playback: State<'_, PlaybackState>,
    mut track: Track,
) -> Result<Track, String> {
    // Resolve the real stream (and its source-reported quality/format) before
    // handing it to the WebView, so the resolved metadata reaches the UI.
    let resolved = resolve_track_for_action(state.client.clone(), track.clone()).await?;
    let token = playback.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    playback
        .tracks
        .lock()
        .map_err(|_| "播放会话不可用".to_string())?
        .insert(token.clone(), resolved.clone());
    // Replace the upstream source URL with the proxied playback URI; the native
    // `music://` handler still fetches from the stored upstream URL.
    track = resolved;
    track.audio_url = format!("music://localhost/{token}");
    Ok(track)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            client: build_client(),
        })
        .manage(PlaybackState {
            tracks: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
        .manage(LocalArtworkState {
            artwork: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
        .manage(DownloadState {
            cancels: Mutex::new(HashMap::new()),
        })
        .register_asynchronous_uri_scheme_protocol("localart", |context, request, responder| {
            let token = request.uri().path().trim_start_matches('/');
            let artwork = context
                .app_handle()
                .state::<LocalArtworkState>()
                .artwork
                .lock()
                .ok()
                .and_then(|images| images.get(token).cloned());
            match artwork {
                Some(artwork) => responder.respond(
                    tauri::http::Response::builder()
                        .status(200)
                        .header("Content-Type", artwork.content_type)
                        .header("Content-Length", artwork.bytes.len().to_string())
                        .header("Cache-Control", "no-store")
                        .body(artwork.bytes)
                        .unwrap(),
                ),
                None => responder.respond(
                    tauri::http::Response::builder()
                        .status(404)
                        .body(Vec::new())
                        .unwrap(),
                ),
            }
        })
        .register_asynchronous_uri_scheme_protocol("music", |context, request, responder| {
            let token = request.uri().path().trim_start_matches('/').to_string();
            let range = request.headers().get(tauri::http::header::RANGE).cloned();
            let track = context
                .app_handle()
                .state::<PlaybackState>()
                .tracks
                .lock()
                .ok()
                .and_then(|tracks| tracks.get(&token).cloned());
            let client = context.app_handle().state::<AppState>().client.clone();
            tauri::async_runtime::spawn(async move {
                let Some(track) = track else {
                    responder.respond(
                        tauri::http::Response::builder()
                            .status(404)
                            .body(Vec::new())
                            .unwrap(),
                    );
                    return;
                };
                if let Some(path) = local_path(&track) {
                    let metadata = match tokio::fs::metadata(&path).await {
                        Ok(metadata) => metadata,
                        Err(_) => {
                            responder.respond(
                                tauri::http::Response::builder()
                                    .status(404)
                                    .body(Vec::new())
                                    .unwrap(),
                            );
                            return;
                        }
                    };
                    let total = metadata.len();
                    let Some((start, end)) = requested_local_range(range.as_ref(), total) else {
                        responder.respond(
                            tauri::http::Response::builder()
                                .status(416)
                                .header("Content-Range", format!("bytes */{total}"))
                                .body(Vec::new())
                                .unwrap(),
                        );
                        return;
                    };
                    let length = (end - start + 1) as usize;
                    let mut file = match tokio::fs::File::open(&path).await {
                        Ok(file) => file,
                        Err(_) => {
                            responder.respond(
                                tauri::http::Response::builder()
                                    .status(404)
                                    .body(Vec::new())
                                    .unwrap(),
                            );
                            return;
                        }
                    };
                    let result = async {
                        file.seek(std::io::SeekFrom::Start(start)).await?;
                        let mut body = vec![0; length];
                        file.read_exact(&mut body).await?;
                        Ok::<_, std::io::Error>(body)
                    }
                    .await;
                    let body = match result {
                        Ok(body) => body,
                        Err(_) => {
                            responder.respond(
                                tauri::http::Response::builder()
                                    .status(500)
                                    .body(Vec::new())
                                    .unwrap(),
                            );
                            return;
                        }
                    };
                    let content_type =
                        media_content_type(&track, &reqwest::header::HeaderMap::new());
                    responder.respond(
                        tauri::http::Response::builder()
                            .status(206)
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Type", content_type)
                            .header("Content-Length", body.len().to_string())
                            .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                            .body(body)
                            .unwrap(),
                    );
                    return;
                }
                let mut source_request = client.get(&track.audio_url);
                // WebKit normally asks for a Range immediately. If it does not,
                // request only the opening segment ourselves so starting playback
                // never waits for a complete lossless-file download.
                let source_range =
                    range.unwrap_or_else(|| tauri::http::HeaderValue::from_static("bytes=0-65535"));
                source_request = source_request.header(reqwest::header::RANGE, source_range);
                if let Some(headers) = track
                    .adapter_payload
                    .as_ref()
                    .and_then(|payload| payload.get("downloadHeaders"))
                    .and_then(|value| value.as_object())
                {
                    for (name, value) in headers {
                        if let Some(value) = value.as_str() {
                            source_request = source_request.header(name, value);
                        }
                    }
                }
                let response = match source_request.send().await {
                    Ok(response) => response,
                    Err(_) => {
                        responder.respond(
                            tauri::http::Response::builder()
                                .status(502)
                                .body(Vec::new())
                                .unwrap(),
                        );
                        return;
                    }
                };
                let status = response.status();
                let headers = response.headers().clone();
                // A source may answer with a 200 HTML/JSON error page (e.g. a
                // blocked or expired link). Serving it as audio makes WebKit
                // "play" silence, so reject it and let the player surface an error.
                let source_type = headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if source_type.contains("text/html")
                    || source_type.contains("application/json")
                    || source_type.contains("application/xml")
                    || source_type.contains("text/plain")
                {
                    responder.respond(
                        tauri::http::Response::builder()
                            .status(502)
                            .body(Vec::new())
                            .unwrap(),
                    );
                    return;
                }
                let content_type = media_content_type(&track, &headers);
                let body = match response.bytes().await {
                    Ok(body) => body.to_vec(),
                    Err(_) => {
                        responder.respond(
                            tauri::http::Response::builder()
                                .status(502)
                                .body(Vec::new())
                                .unwrap(),
                        );
                        return;
                    }
                };
                let mut builder = tauri::http::Response::builder()
                    .status(status)
                    .header("Accept-Ranges", "bytes")
                    .header("Content-Type", content_type)
                    .header("Content-Length", body.len().to_string());
                if let Some(content_range) = headers.get(reqwest::header::CONTENT_RANGE) {
                    builder = builder.header("Content-Range", content_range);
                }
                responder.respond(builder.body(body).unwrap());
            });
        })
        .invoke_handler(tauri::generate_handler![
            list_sources,
            search_tracks,
            resolve_qualities,
            scan_local_music,
            download_track,
            cancel_download,
            delete_file,
            get_lyrics,
            load_download_history,
            save_download_history,
            prepare_playback
        ])
        .run(tauri::generate_context!())
        .expect("error while running LiteMusicDL");
}

#[cfg(test)]
mod tests {
    use super::{has_audio_magic, is_lyric_file, local_lyrics_path, local_track, sniff_image_type};
    use std::fs;

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("litemusicdl-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn excludes_gbk_lrc_renamed_as_audio() {
        let path = fixture_path("lyrics.mp3");
        fs::write(&path, b"[00:01.00]\xd6\xd0\xce\xc4\xb8\xe8\xb4\xca").unwrap();
        assert!(is_lyric_file(&path));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn excludes_utf8_bom_lrc_and_companion_types() {
        let path = fixture_path("bom.lrc");
        fs::write(&path, b"\xef\xbb\xbf[00:01.00]lyric").unwrap();
        assert!(is_lyric_file(&path));
        fs::write(&path, b"[00:01.00]x").unwrap();
        fs::remove_file(path).unwrap();

        for name in ["song.txt", "song.cue", "song.m3u", "song.nfo"] {
            let candidate = fixture_path(name);
            fs::write(&candidate, b"anything").unwrap();
            assert!(is_lyric_file(&candidate), "{name} should be excluded");
            fs::remove_file(candidate).unwrap();
        }
    }

    #[test]
    fn audio_magic_is_not_lyric() {
        let path = fixture_path("audio.mp3");
        fs::write(&path, b"ID3\x04\x00\x00\x00\x00\x00\x00tag").unwrap();
        assert!(!is_lyric_file(&path));
        assert!(has_audio_magic(&path));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sniffs_embedded_image_types() {
        assert_eq!(sniff_image_type(&[0xff, 0xd8, 0xff, 0xdb, 0x00, 0x00]), Some("image/jpeg"));
        assert_eq!(
            sniff_image_type(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            Some("image/png")
        );
        assert_eq!(sniff_image_type(&[0x00, 0x01, 0x02]), None);
    }

    #[test]
    fn rejects_plain_text_renamed_as_audio() {
        let path = fixture_path("fake2.mp3");
        fs::write(&path, b"just a plain text file").unwrap();
        assert!(!is_lyric_file(&path));
        assert!(!has_audio_magic(&path));
        // A file loftily cannot parse and without audio magic must not become a song.
        assert!(local_track(&path).is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finds_case_insensitive_companion_lrc() {
        let music = fixture_path("song.flac");
        let lyrics = fixture_path("song.LRC");
        fs::write(&music, b"fLaC").unwrap();
        fs::write(&lyrics, b"[00:01.00]lyric").unwrap();
        let found = local_lyrics_path(&music).expect("companion LRC should be found");
        assert_eq!(fs::read(found).unwrap(), b"[00:01.00]lyric");
        fs::remove_file(music).unwrap();
        fs::remove_file(lyrics).unwrap();
    }
}

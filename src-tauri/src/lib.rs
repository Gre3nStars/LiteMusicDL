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
    match normalized.as_str() {
        // Kuwo's active fallback returns `audio/x-flac`. WebKit's custom URI
        // media path recognises the standard form reliably, while it may reject
        // the legacy x- prefix with MEDIA_ERR_SRC_NOT_SUPPORTED (error 4).
        "audio/x-flac" | "application/flac" | "application/x-flac" => return "audio/flac".into(),
        "audio/x-wav" | "audio/wave" => return "audio/wav".into(),
        "audio/x-m4a" | "audio/mp4" | "video/mp4" => return "audio/mp4".into(),
        "audio/x-ogg" => return "audio/ogg".into(),
        _ if normalized.starts_with("audio/") => return normalized,
        _ => {}
    }
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
    match extension.as_str() {
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        _ => "audio/mpeg",
    }
    .into()
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

fn is_lyric_file(path: &std::path::Path) -> bool {
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lrc"))
    {
        return true;
    }
    let Ok(contents) = std::fs::read(path) else {
        return false;
    };
    if contents.starts_with(&[0xff, 0xfe]) || contents.starts_with(&[0xfe, 0xff]) {
        return true;
    }
    let contents = &contents[..contents.len().min(4_096)];
    // A number of LRC files use GBK, so checking UTF-8 validity here would
    // incorrectly let a renamed `*.lrc.mp3` text file into the music list.
    contents.starts_with(b"[") && contents[..contents.len().min(256)].contains(&b']')
}

fn local_artwork_from_bytes(content_type: &str, bytes: Vec<u8>) -> Option<LocalArtwork> {
    if !matches!(
        content_type,
        "image/jpeg" | "image/png" | "image/gif" | "image/bmp" | "image/webp"
    ) || bytes.is_empty()
        || bytes.len() > 12 * 1024 * 1024
    {
        return None;
    }
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
    let content_type = picture.mime_type()?.as_str();
    local_artwork_from_bytes(content_type, picture.data().to_vec())
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
            (
                descriptor.name,
                adapter.search(&query, limit.max(1) as usize).await,
            )
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

async fn resolve_track_for_action(client: reqwest::Client, track: Track) -> Result<Track, String> {
    if !track.audio_url.trim().is_empty() || local_path(&track).is_some() {
        return Ok(track);
    }
    let source = registry(client)
        .into_iter()
        .find(|source| source.descriptor().id == track.source)
        .ok_or_else(|| format!("未找到 {} 的音源适配器", track.source))?;
    source.resolve_track(&track).await
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
    track: Track,
    directory: Option<String>,
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
    let path = download::download_track(&state.client, &track, &base).await?;

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
    track: Track,
) -> Result<String, String> {
    let track = resolve_track_for_action(state.client.clone(), track).await?;
    let token = playback.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    playback
        .tracks
        .lock()
        .map_err(|_| "播放会话不可用".to_string())?
        .insert(token.clone(), track);
    Ok(format!("music://localhost/{token}"))
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
            scan_local_music,
            download_track,
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
    use super::{is_lyric_file, local_lyrics_path};
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

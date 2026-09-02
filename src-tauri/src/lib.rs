mod domain;
mod download;
mod sources;

use crate::domain::{SourceDescriptor, Track};
use futures_util::future::join_all;
use futures_util::StreamExt;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::Accessor;
use serde_json::{json, Value};
use sources::registry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

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

/// The loopback media server's bound port plus the shared playback map.
/// `prepare_playback` inserts the resolved track into `tracks` and emits an
/// `http://127.0.0.1:{port}/{token}` URL (WebView2 refuses custom-scheme media
/// with error 4, but streams `http://127.0.0.1` reliably). The server reads the
/// same map to stream bytes back to the WebView.
struct MediaServerState {
    port: u16,
    tracks: Arc<Mutex<HashMap<String, Track>>>,
}

#[derive(Clone)]
struct LocalArtwork {
    content_type: String,
    bytes: Vec<u8>,
}

/// The artwork map is shared (via `Arc`) with the loopback media server so local
/// cover art can be served over `http://127.0.0.1:{port}/art/{token}`, which
/// WebView2 loads reliably. The old `localart://` custom scheme is refused by
/// WebView2 for images, so covers silently failed on Windows.
struct LocalArtworkState {
    artwork: Arc<Mutex<HashMap<String, LocalArtwork>>>,
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

/// Parse the browser's `Range: bytes=start-end` header for the remote proxy,
/// where the full size is unknown ahead of time. Returns `(start, end)` or
/// `None` when the header is absent/malformed. Open-ended ranges (`bytes=start-`)
/// are clamped to a generous window so we don't download the whole file.
fn requested_range(range: Option<&tauri::http::HeaderValue>) -> Option<(usize, usize)> {
    const WINDOW: usize = 65_536;
    let raw = range.and_then(|value| value.to_str().ok())?;
    let raw = raw.trim().strip_prefix("bytes=")?;
    let (start, end) = raw.split(',').next()?.split_once('-')?;
    if start.is_empty() {
        // suffix range `bytes=-N` -> last N bytes; approximate the tail.
        let length = end.parse::<usize>().ok()?;
        return Some((WINDOW.saturating_sub(length), WINDOW - 1));
    }
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok().unwrap_or(start + WINDOW);
    Some((start, end.max(start)))
}

/// Build the HTTP response for a streamed track. Shared by the loopback media
/// server and the custom `music://` protocol so both behave identically and
/// serve correct Range/Content-Type headers across WKWebView and WebView2.
struct StreamResponse {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    content_range: Option<String>,
}

async fn stream_track(
    client: reqwest::Client,
    track: &Track,
    range: Option<&tauri::http::HeaderValue>,
) -> StreamResponse {
    // Local file: serve the requested byte window directly.
    if let Some(path) = local_path(track) {
        if let Ok(metadata) = tokio::fs::metadata(&path).await {
            let total = metadata.len();
            let Some((start, end)) = requested_local_range(range, total) else {
                return StreamResponse {
                    status: 416,
                    content_type: String::new(),
                    body: Vec::new(),
                    content_range: Some(format!("bytes */{total}")),
                };
            };
            let length = (end - start + 1) as usize;
            let body = match tokio::fs::File::open(&path).await {
                Ok(mut file) => {
                    let result = async {
                        file.seek(std::io::SeekFrom::Start(start)).await?;
                        let mut buf = vec![0; length];
                        file.read_exact(&mut buf).await?;
                        Ok::<_, std::io::Error>(buf)
                    }
                    .await;
                    result.unwrap_or_default()
                }
                Err(_) => Vec::new(),
            };
            return StreamResponse {
                status: 206,
                content_type: media_content_type(track, &reqwest::header::HeaderMap::new()),
                body,
                content_range: Some(format!("bytes {start}-{end}/{total}")),
            };
        }
    }

    // Remote source: fetch the upstream URL (with Range + download headers).
    let source_range = range.cloned().unwrap_or_else(|| {
        tauri::http::HeaderValue::from_static("bytes=0-65535")
    });
    let mut source_request = client.get(&track.audio_url);
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
    let Ok(response) = source_request.send().await else {
        return StreamResponse {
            status: 502,
            content_type: String::new(),
            body: Vec::new(),
            content_range: None,
        };
    };
    let source_status = response.status();
    let headers = response.headers().clone();
    let source_total = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    // A blocked/expired link answering with an HTML/JSON error page is never audio.
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
        return StreamResponse {
            status: 502,
            content_type: String::new(),
            body: Vec::new(),
            content_range: None,
        };
    }
    let content_type = media_content_type(track, &headers);
    let requested = requested_range(range);
    let mut status = source_status.as_u16();
    let mut body;
    let mut content_range = None;
    if let Some((start, end)) = requested {
        // Stream only the requested byte window from the upstream so the first
        // bytes reach the WebView promptly. Reading the whole body first (as a
        // source that ignores `Range` can return) stalls a slow/large stream and
        // trips the frontend's stall watchdog. If the upstream honoured our
        // range (206) the body already begins at `start`; otherwise (200) we
        // skip `start` bytes before copying the window.
        let window = end - start + 1;
        let is_partial = source_status == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut skip = if is_partial { 0usize } else { start };
        let mut stream = response.bytes_stream();
        body = Vec::with_capacity(window.min(1 << 20));
        while body.len() < window {
            let chunk = match stream.next().await {
                Some(Ok(chunk)) => chunk,
                _ => break,
            };
            let cursor = if skip > 0 {
                let take = chunk.len().min(skip);
                skip -= take;
                &chunk[take..]
            } else {
                &chunk[..]
            };
            if cursor.is_empty() {
                continue;
            }
            let take = cursor.len().min(window - body.len());
            body.extend_from_slice(&cursor[..take]);
        }
        // A full 200 body that ended before `start` cannot satisfy the request.
        if !is_partial && skip > 0 {
            return StreamResponse {
                status: 416,
                content_type: String::new(),
                body: Vec::new(),
                content_range: None,
            };
        }
        status = 206;
        // Report the real total so the media element can seek and report duration;
        // prefer the upstream Content-Length, then its Content-Range total.
        let total = source_total
            .or_else(|| {
                headers
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.rsplit('/').next())
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or_else(|| start + body.len());
        // Describe the bytes actually served (the file may be shorter than the
        // requested window), so the media element never sees an oversized range.
        if body.is_empty() {
            content_range = Some(format!("bytes {start}-{}/{total}", start.saturating_sub(1)));
        } else {
            content_range = Some(format!("bytes {start}-{}/{total}", start + body.len() - 1));
        }
    } else {
        body = match response.bytes().await {
            Ok(body) => body.to_vec(),
            Err(_) => {
                return StreamResponse {
                    status: 502,
                    content_type: String::new(),
                    body: Vec::new(),
                    content_range: None,
                };
            }
        };
        if let Some(value) = headers.get(reqwest::header::CONTENT_RANGE) {
            content_range = Some(value.to_str().unwrap_or_default().to_owned());
        }
    }
    StreamResponse {
        status,
        content_type,
        body,
        content_range,
    }
}

/// Bind a loopback HTTP listener and serve `GET /{token}` by streaming the
/// stored track. Returns the accept-loop handle and the bound port. Port `0`
/// lets the OS pick a free one (no conflicts with other apps). Only
/// `127.0.0.1` is bound, so it is never reachable from the network.
fn spawn_media_server(
    client: reqwest::Client,
    tracks: Arc<Mutex<HashMap<String, Track>>>,
    artwork: Arc<Mutex<HashMap<String, LocalArtwork>>>,
) -> (tauri::async_runtime::JoinHandle<()>, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind media server");
    listener
        .set_nonblocking(true)
        .expect("failed to set media server non-blocking");
    let port = listener.local_addr().expect("media server port").port();
    let handle = tauri::async_runtime::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener)
            .expect("failed to adopt media server listener");
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let tracks = tracks.clone();
            let client = client.clone();
            let artwork = artwork.clone();
            tauri::async_runtime::spawn(async move {
                let request = match read_http_request(&mut stream).await {
                    Some(request) => request,
                    None => return,
                };
                let path = request.path.trim_start_matches('/').to_string();
                // Local artwork is served over loopback HTTP (not the localart://
                // custom scheme), because WebView2 refuses custom-scheme images.
                if let Some(token) = path.strip_prefix("art/") {
                    let image = artwork
                        .lock()
                        .ok()
                        .and_then(|images| images.get(token).cloned());
                    match image {
                        Some(image) => {
                            let _ = write_http_response_ext(
                                &mut stream,
                                200,
                                &image.content_type,
                                &image.bytes,
                                None,
                            )
                            .await;
                        }
                        None => {
                            let _ = write_http_response(&mut stream, 404, "text/plain", &[]).await;
                        }
                    }
                    return;
                }
                let track = tracks.lock().ok().and_then(|map| map.get(&path).cloned());
                let Some(track) = track else {
                    let _ = write_http_response(&mut stream, 404, "text/plain", &[]).await;
                    return;
                };
                let res = stream_track(client, &track, request.range.as_ref()).await;
                let body = res.body;
                // We already trimmed to the requested window in `stream_track`;
                // `content_range` (if any) describes the returned slice.
                let _ = write_http_response_ext(
                    &mut stream,
                    res.status,
                    &res.content_type,
                    &body,
                    res.content_range.as_deref(),
                )
                .await;
            });
        }
    });
    (handle, port)
}

struct HttpRequest {
    path: String,
    range: Option<tauri::http::HeaderValue>,
}

/// Read and parse a minimal HTTP/1.1 request head (request line + headers).
async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<HttpRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        // Headers end at the first blank line (\r\n\r\n or \n\n).
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    let range = lines
        .find_map(|line| {
            let line = line.trim();
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("range:")
                .map(|value| tauri::http::HeaderValue::from_str(value.trim()).ok())
                .flatten()
        });
    Some(HttpRequest {
        path: target.to_string(),
        range,
    })
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_http_response_ext(stream, status, content_type, body, None).await
}

async fn write_http_response_ext(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    content_range: Option<&str>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n",
        body.len()
    );
    if let Some(cr) = content_range {
        response.push_str(&format!("Content-Range: {cr}\r\n"));
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
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
    media: State<'_, MediaServerState>,
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
                // WebView2 refuses images from the `localart://` custom scheme, so
                // route cover art through the loopback HTTP server like audio.
                track.artwork_url = format!("http://127.0.0.1:{}/art/{token}", media.port);
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

/// Open the OS file manager at the given file/folder, selecting the item when
/// the platform supports it (Finder / Explorer). Used by the "打开所在文件夹"
/// action on downloads and local-music rows.
#[tauri::command]
async fn reveal_in_folder(path: String) -> Result<(), String> {
    let path = PathBuf::from(path.trim());
    if !path.exists() {
        return Err("文件不存在".into());
    }
    tauri::async_runtime::spawn_blocking(move || reveal_in_folder_os(&path))
        .await
        .map_err(|error| format!("打开所在文件夹任务中断: {error}"))?
}

fn reveal_in_folder_os(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|error| format!("无法在访达中定位文件: {error}"))?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        if path.is_dir() {
            std::process::Command::new("explorer.exe")
                .arg(path)
                .spawn()
                .map_err(|error| format!("无法在资源管理器中打开目录: {error}"))?;
        } else {
            std::process::Command::new("explorer.exe")
                .arg(format!("/select,{}", path.display()))
                .spawn()
                .map_err(|error| format!("无法在资源管理器中定位文件: {error}"))?;
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let target = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .map(|parent| parent.to_path_buf())
                .ok_or_else(|| "无法确定所在文件夹".to_string())?
        };
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map_err(|error| format!("无法打开所在文件夹: {error}"))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        Err("当前平台不支持打开所在文件夹".into())
    }
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
    media: State<'_, MediaServerState>,
    mut track: Track,
) -> Result<Track, String> {
    // Resolve the real stream (and its source-reported quality/format) before
    // handing it to the WebView, so the resolved metadata reaches the UI.
    let resolved = resolve_track_for_action(state.client.clone(), track.clone()).await?;
    let token = playback.next_id.fetch_add(1, Ordering::Relaxed).to_string();
    media
        .tracks
        .lock()
        .map_err(|_| "播放会话不可用".to_string())?
        .insert(token.clone(), resolved.clone());
    // Replace the upstream URL with a loopback HTTP URL served by the media
    // server. WebView2/Chromium refuses media from a custom scheme (error 4)
    // but plays `http://127.0.0.1` reliably, so we route playback over a real
    // loopback HTTP listener rather than the `music://` protocol.
    track = resolved;
    track.audio_url = format!("http://127.0.0.1:{}/{token}", media.port);
    Ok(track)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let client = build_client();
    let tracks = Arc::new(Mutex::new(HashMap::new()));

    // Start a loopback HTTP media server. WebView2 plays `http://127.0.0.1`
    // media reliably, so the frontend streams through this listener instead of
    // the custom `music://` scheme (which Chromium rejects with error 4).
    let artwork = Arc::new(Mutex::new(HashMap::new()));
    let (server, port) = spawn_media_server(client.clone(), tracks.clone(), artwork.clone());
    let media_server = MediaServerState { port, tracks };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            client: client.clone(),
        })
        .manage(PlaybackState {
            tracks: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
        .manage(media_server)
        .manage(LocalArtworkState {
            artwork,
            next_id: AtomicU64::new(1),
        })
        .manage(DownloadState {
            cancels: Mutex::new(HashMap::new()),
        })
        .setup(|_app| {
            // Keep the server alive for the lifetime of the app.
            std::mem::forget(server);
            Ok(())
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
                let res = stream_track(client, &track, range.as_ref()).await;
                let mut builder = tauri::http::Response::builder()
                    .status(res.status)
                    .header("Accept-Ranges", "bytes")
                    .header("Content-Type", res.content_type)
                    .header("Content-Length", res.body.len().to_string());
                if let Some(cr) = res.content_range {
                    builder = builder.header("Content-Range", cr);
                }
                responder.respond(builder.body(res.body).unwrap());
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
            reveal_in_folder,
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
    use super::{
        has_audio_magic, is_lyric_file, local_lyrics_path, local_track, media_content_type,
        stream_track, sniff_image_type,
    };
    use crate::domain::Track;
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

    fn track(format: &str) -> Track {
        Track {
            id: "qq:test".into(),
            source: "QQMusicClient".into(),
            title: "t".into(),
            artist: String::new(),
            album: String::new(),
            artwork_url: String::new(),
            audio_url: "https://x/foo.flac".into(),
            duration_ms: 0,
            format: Some(format.into()),
            quality: None,
            adapter_payload: None,
        }
    }

    fn headers_with(content_type: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_str(content_type).unwrap(),
        );
        headers
    }

    // QQ/HK0 love to serve FLAC bytes as `audio/x-ogg`, which the WebView rejects
    // with MEDIA_ERR_SRC_NOT_SUPPORTED (error 4). The proxy must re-label by the
    // resolved container, not echo the bogus header.
    #[test]
    fn media_content_type_trusts_container_over_bogus_ogg_header() {
        let headers = headers_with("audio/x-ogg");
        assert_eq!(media_content_type(&track("flac"), &headers), "audio/flac");
        assert_eq!(media_content_type(&track("FLAC"), &headers), "audio/flac");
        // A real OGG keeps its own type.
        assert_eq!(media_content_type(&track("ogg"), &headers), "audio/ogg");
    }

    #[test]
    fn media_content_type_maps_lossless_mislabel_as_mpeg() {
        // Netease/QQ can deliver FLAC bytes labelled audio/mpeg.
        let headers = headers_with("audio/mpeg");
        assert_eq!(media_content_type(&track("flac"), &headers), "audio/flac");
        // Genuine MP3 stays MP3.
        assert_eq!(media_content_type(&track("mp3"), &headers), "audio/mpeg");
    }

    fn range_header(value: &str) -> tauri::http::HeaderValue {
        tauri::http::HeaderValue::from_str(value).unwrap()
    }

    #[test]
    fn requested_range_parses_explicit_and_open_ended() {
        // Explicit `bytes=0-1023`.
        assert_eq!(
            super::requested_range(Some(&range_header("bytes=0-1023"))),
            Some((0, 1023))
        );
        // Open-ended `bytes=0-` clamps to a window instead of downloading all bytes.
        let (start, end) = super::requested_range(Some(&range_header("bytes=100-"))).unwrap();
        assert_eq!(start, 100);
        assert!(end >= start);
        // Missing / malformed header -> None (serve full body).
        assert_eq!(super::requested_range(None), None);
        assert_eq!(super::requested_range(Some(&range_header("garbage"))), None);
    }

    #[tokio::test]
    async fn stream_track_serves_local_file_range() {
        // Write a small local FLAC file and build its Track through local_track.
        let path = fixture_path("media.flac");
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&[0u8; 100]);
        fs::write(&path, &bytes).unwrap();
        let (track, _art) = local_track(&path).expect("local_track should detect the flac");

        let client = reqwest::Client::new();
        let range = range_header("bytes=0-15");
        let res = stream_track(client, &track, Some(&range)).await;
        assert_eq!(res.status, 206);
        assert_eq!(res.content_type, "audio/flac");
        assert_eq!(res.body.len(), 16);
        assert_eq!(&res.body[..4], b"fLaC");
        assert!(res.content_range.as_deref().unwrap_or("").starts_with("bytes 0-15/"));

        fs::remove_file(path).unwrap();
    }
}

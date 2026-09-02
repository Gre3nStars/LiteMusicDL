use super::MusicSource;
use crate::domain::{SourceDescriptor, Track};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

const QQ_API: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const QQ_STREAM: &str = "https://isure.stream.qqmusic.qq.com/";
const QQ_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const QQ_REFERER: &str = "https://y.qq.com/";
const QQ_SOURCE: &str = "QQMusicClient";

static GUID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct QqSource {
    client: reqwest::Client,
}

impl QqSource {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    fn common() -> Value {
        json!({
            "ct": "11", "tmeAppID": "qqmusic", "format": "json",
            "inCharset": "utf-8", "outCharset": "utf-8", "uid": "3931641530",
            "cv": 13020508, "v": 13020508,
            // musicdl's documented fallback when QIMEI acquisition is unavailable.
            "QIMEI36": "6c9d3cd110abca9b16311cee10001e717614"
        })
    }

    fn guid() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        format!(
            "{:032x}",
            now ^ GUID_COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn legacy_search(&self, query: &str, limit: usize, page: u32) -> Result<Vec<Value>, String> {
        let response = self
            .client
            .get("https://c.y.qq.com/soso/fcgi-bin/client_search_cp")
            .query(&[
                ("format", "json"),
                ("p", &page.to_string()),
                ("n", &limit.to_string()),
                ("w", query),
            ])
            .header("User-Agent", QQ_UA)
            .header("Referer", QQ_REFERER)
            .send()
            .await
            .map_err(|error| format!("QQ 音乐兼容搜索请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("QQ 音乐兼容搜索返回错误: {error}"))?;
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| format!("QQ 音乐兼容搜索解析失败: {error}"))?;
        let mut songs = body
            .pointer("/data/song/list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // The legacy response keeps album fields flat, while the mobile
        // response nests them. Normalize only that shape before using the
        // common QqSong parser.
        for song in &mut songs {
            if song.get("album").is_none() {
                song["album"] = json!({
                    "title": song.get("albumname").and_then(Value::as_str).unwrap_or_default(),
                    "mid": song.get("albummid").and_then(Value::as_str).unwrap_or_default(),
                });
            }
        }
        Ok(songs)
    }

    /// Full resolution entry point. musicdl's QQ `_parsewiththirdpartapis` tries a
    /// *list* of third-party parsers so a single rate-limited/blocked host does
    /// not fail playback. All resolvers run concurrently so a slow/hung endpoint
    /// never blocks the fast ones; candidates are ranked by quality, then each is
    /// probed for a real audio container before being accepted (also filtering out
    /// dead/mislabeled URLs that surface as MEDIA_ERR_SRC_NOT_SUPPORTED, error 4).
    async fn resolve_stream(&self, song_mid: &str) -> (String, String, String) {
        use futures_util::future::{join_all, BoxFuture};
        let resolvers: Vec<BoxFuture<'_, Option<(String, String, String)>>> = vec![
            Box::pin(self.resolve_vkey(song_mid)),
            Box::pin(self.resolve_tang(song_mid)),
            Box::pin(self.resolve_hk0cc(song_mid)),
            Box::pin(self.resolve_lzmhhh(song_mid)),
            Box::pin(self.resolve_yutangxiaowu(song_mid)),
        ];
        let mut results = join_all(resolvers)
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        // Best available source wins: 无损/FLAC beats 320K beats 128K. The proxy
        // re-labels the container correctly, so lossless plays everywhere it's
        // decodable — we never trade down the quality the source offers.
        results.sort_by(|a, b| quality_rank(&b.1).cmp(&quality_rank(&a.1)));
        for (url, quality, extension) in results {
            if !url.is_empty() && self.streamable(&url, &extension).await {
                return (url, quality, extension);
            }
        }
        (String::new(), String::new(), String::new())
    }

    /// Soundness check: confirm the URL serves a real audio container, not an
    /// HTML/JSON error page or a dead link. This is what prevents the WebView
    /// from reporting MEDIA_ERR_SRC_NOT_SUPPORTED (error 4) when a resolver hands
    /// back a "successful" but non-audio URL. Fast path: a recognised audio
    /// Content-Type (206) is accepted without buffering; only the known-mislabel
    /// `audio/x-ogg` case reads the leading bytes to trust the real container.
    async fn streamable(&self, url: &str, extension: &str) -> bool {
        let response = match self
            .client
            .get(url)
            .timeout(Duration::from_secs(5))
            .header("User-Agent", QQ_UA)
            .header("Referer", "http://y.qq.com")
            .header("Range", "bytes=0-2047")
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return false,
        };
        let status = response.status();
        if !(status.is_success() || status.as_u16() == 206) {
            return false;
        }
        let kind = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // An HTML/JSON error page is never audio.
        if kind.contains("text/html")
            || kind.contains("application/json")
            || kind.contains("text/plain")
        {
            return false;
        }
        // Fast path: a real audio type is accepted without reading the body.
        let base_type = kind.split(';').next().unwrap_or_default();
        if matches!(
            base_type,
            "audio/mpeg" | "audio/mp4" | "audio/aac" | "audio/ogg" | "audio/flac"
        ) {
            return true;
        }
        // `audio/x-ogg` is how QQ serves genuine FLAC bytes (verified: fLaC magic
        // with an audio/x-ogg header). Re-label by the container, else reject so the
        // caller falls through to a reliable MP3/M4A resolver instead of error 4.
        if base_type == "audio/x-ogg" {
            if let Ok(bytes) = response.bytes().await {
                let sample = &bytes[..bytes.len().min(64)];
                let ext = extension.to_ascii_lowercase();
                let is_flac = sample.starts_with(b"fLaC");
                let is_ogg = sample.starts_with(b"OggS");
                if is_flac && ext != "ogg" {
                    return true;
                }
                return is_ogg;
            }
            return false;
        }
        // Unknown header: trust the container magic if present.
        if let Ok(bytes) = response.bytes().await {
            let sample = &bytes[..bytes.len().min(64)];
            let ext = extension.to_ascii_lowercase();
            let magic_ok = if ext == "flac" {
                sample.starts_with(b"fLaC")
            } else if ext == "mp3" {
                sample.starts_with(b"ID3")
                    || (sample[0] == 0xff && sample.get(1).is_some_and(|b| b & 0xe0 == 0xe0))
            } else if ext == "m4a" {
                sample.get(4..8) == Some(b"ftyp")
            } else {
                false
            };
            return magic_ok;
        }
        false
    }

    /// Official `music.vkey.GetVkey.UrlGetVkey` path (musicdl `_parsewithofficialapiv1`).
    /// Only the realistic guest tiers are probed (lossless/mp3/aac); the VIP-only
    /// "臻品" and "全景声" tiers never yield a purl for an anonymous request, and
    /// skipping them keeps this path from undercutting the faster fallbacks.
    async fn resolve_vkey(&self, song_mid: &str) -> Option<(String, String, String)> {
        const GUEST_TYPES: &[(&str, &str, &str)] = &[
            ("F000", ".flac", "无损"),
            ("M800", ".mp3", "MP3 320K"),
            ("M500", ".mp3", "MP3 128K"),
            ("C600", ".m4a", "AAC 192K"),
        ];
        for (prefix, extension, quality) in GUEST_TYPES {
            let filename = format!("{prefix}{song_mid}{song_mid}{extension}");
            let payload = json!({
                "comm": Self::common(),
                "music.vkey.GetVkey.UrlGetVkey": {
                    "module": "music.vkey.GetVkey", "method": "UrlGetVkey",
                    "param": {"filename": [filename], "guid": Self::guid(), "songmid": [song_mid], "songtype": [0]}
                }
            });
            let result = self
                .client
                .post(QQ_API)
                .timeout(Duration::from_secs(4))
                .header("User-Agent", QQ_UA)
                .header("Referer", QQ_REFERER)
                .header("Origin", QQ_REFERER.trim_end_matches('/'))
                .json(&payload)
                .send()
                .await
                .ok()
                .and_then(|response| response.error_for_status().ok());
            let Some(response) = result else { continue };
            let Ok(value) = response.json::<Value>().await else {
                continue;
            };
            let purl = value
                .pointer("/music.vkey.GetVkey.UrlGetVkey/data/midurlinfo/0/purl")
                .or_else(|| {
                    value.pointer("/music.vkey.GetVkey.UrlGetVkey/data/midurlinfo/0/wifiurl")
                })
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !purl.is_empty() {
                let url = if purl.starts_with("http") {
                    purl.to_owned()
                } else {
                    format!("{QQ_STREAM}{purl}")
                };
                return Some((
                    url,
                    (*quality).to_owned(),
                    extension.trim_start_matches('.').to_owned(),
                ));
            }
        }
        None
    }

    // musicdl's `_parsewithtangapi` — shared shape with hk0cc (same backend).
    async fn resolve_tang(&self, song_mid: &str) -> Option<(String, String, String)> {
        self.tanglike("https://tang.api.s01s.cn/music_open_api.php", song_mid).await
    }

    // musicdl's `_parsewithhk0ccapi` — an independent host with the same shape.
    async fn resolve_hk0cc(&self, song_mid: &str) -> Option<(String, String, String)> {
        self.tanglike("https://api.hk0.cc/api/qqmusic", song_mid).await
    }

    /// Shared parser for the tang/hk0cc family, which return `song_play_url_{sq,hq,...}`.
    /// Order: lossless -> high -> standard so a lossless-capable song plays as 无损.
    async fn tanglike(&self, base: &str, song_mid: &str) -> Option<(String, String, String)> {
        let value = self
            .client
            .get(base)
            .timeout(Duration::from_secs(8))
            .query(&[("mid", song_mid)])
            .header("User-Agent", QQ_UA)
            .header("Referer", QQ_REFERER)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        let candidates = [
            ("song_play_url_sq", "无损"),
            ("song_play_url_pq", "高品质"),
            ("song_play_url_accom", "伴奏"),
            ("song_play_url_hq", "320K"),
            ("song_play_url", "128K"),
            ("song_play_url_standard", "128K"),
            ("song_play_url_fq", "低品质"),
        ];
        for (field, quality) in candidates {
            let Some(url) = value
                .get(field)
                .and_then(Value::as_str)
                .filter(|url| url.starts_with("http"))
            else {
                continue;
            };
            let extension = url
                .split('?')
                .next()
                .and_then(|url| url.rsplit('.').next())
                .filter(|value| value.len() <= 5)
                .unwrap_or("m4a");
            return Some((url.into(), quality.into(), extension.into()));
        }
        None
    }

    // musicdl's `_parsewithlzmhhhapi` — POST, returns a plain MP3 URL (universally
    // decodable, so a reliable last-resort that avoids FLAC/WebKit codec issues).
    async fn resolve_lzmhhh(&self, song_mid: &str) -> Option<(String, String, String)> {
        let value = self
            .client
            .post("https://music.lzmhhh.com/api/music/url")
            .timeout(Duration::from_secs(8))
            .header("User-Agent", QQ_UA)
            .header("Referer", "https://music.lzmhhh.com/")
            .header("Origin", "https://music.lzmhhh.com")
            .form(&[("id", song_mid.to_string()), ("type", "qq".to_string())])
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        let url = value.get("data").and_then(Value::as_str)?;
        if !url.starts_with("http") {
            return None;
        }
        Some((url.to_string(), "320K".to_string(), "mp3".to_string()))
    }

    // musicdl's `_parsewithyutangxiaowuapi` — returns an MP3 URL for guests.
    async fn resolve_yutangxiaowu(&self, song_mid: &str) -> Option<(String, String, String)> {
        let value = self
            .client
            .get("https://api.yutangxiaowu.cn/api/v1/qqmusic/music")
            .timeout(Duration::from_secs(8))
            .query(&[("songmid", song_mid)])
            .header("User-Agent", QQ_UA)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        let url = value.get("url").and_then(Value::as_str)?;
        if !url.starts_with("http") {
            return None;
        }
        Some((url.to_string(), "320K".to_string(), "mp3".to_string()))
    }

    async fn query_lyric(&self, song_mid: &str) -> Option<String> {
        let value = self
            .client
            .get("https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg")
            .timeout(Duration::from_secs(8))
            .query(&[
                ("songmid", song_mid),
                ("g_tk", "5381"),
                ("loginUin", "0"),
                ("hostUin", "0"),
                ("format", "json"),
                ("inCharset", "utf8"),
                ("outCharset", "utf-8"),
                ("platform", "yqq"),
            ])
            .header("User-Agent", QQ_UA)
            .header("Referer", "https://y.qq.com/portal/player.html")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<QqLyricEnvelope>()
            .await
            .ok()?;
        value
            .lyric
            .and_then(|encoded| decode_base64(&encoded))
            .filter(|text| !text.trim().is_empty())
    }

    fn track_from_song(song: QqSong) -> Track {
        let (quality, format) = qq_quality(&song);
        Track {
            id: format!("qq:{}", song.mid),
            source: QQ_SOURCE.into(),
            title: song.title,
            artist: song
                .singer
                .into_iter()
                .map(|singer| singer.name)
                .collect::<Vec<_>>()
                .join(" / "),
            album: song.album.title,
            artwork_url: format!(
                "https://y.gtimg.cn/music/photo_new/T002R800x800M000{}.jpg",
                song.album.mid
            ),
            audio_url: String::new(),
            duration_ms: song.interval * 1000,
            format,
            quality,
            adapter_payload: Some(
                json!({"downloadHeaders": {"Referer": "http://y.qq.com", "User-Agent": QQ_UA}}),
            ),
        }
    }
}

/// Higher number = better quality, used to prefer the best URL among resolvers
/// that all returned candidates. Mirrors musicdl's QQ quality ordering.
fn quality_rank(quality: &str) -> u8 {
    match quality {
        "臻品母带" => 8,
        "无损" => 7,
        "高品质" => 6,
        "伴奏" => 5,
        "320K" => 4,
        "128K" => 3,
        "低品质" => 2,
        _ => 1,
    }
}

/// The quality reported by the search card, aligned with what `resolve_stream`
/// actually chooses (lossless first). Reads BOTH the mobile `file.size_*` object
/// and the legacy flat `size128/size320/sizeflac/sizeape/sizeogg` fields, so
/// quality shows even when the mobile API returns empty and we fall back to
/// `client_search_cp`. Dolby / Hi-Res are never resolve targets.
fn qq_quality(song: &QqSong) -> (Option<String>, Option<String>) {
    let f = &song.file;
    if f.size_flac > 0 || f.size_ape > 0 || song.sizeflac > 0 || song.sizeape > 0 {
        (Some("无损".into()), Some("FLAC".into()))
    } else if f.size_320mp3 > 0 || song.size320 > 0 || song.sizeogg > 0 {
        (Some("320K".into()), Some("MP3".into()))
    } else if f.size_192aac > 0 || f.size_192ogg > 0 {
        (Some("192K".into()), Some("MP3".into()))
    } else if f.size_128mp3 > 0 || song.size128 > 0 {
        (Some("128K".into()), Some("MP3".into()))
    } else if f.size_96aac > 0 || f.size_96ogg > 0 {
        (Some("96K".into()), Some("MP3".into()))
    } else if f.size_24aac > 0 {
        (Some("48K".into()), Some("AAC".into()))
    } else {
        (None, None)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct QqSong {
    #[serde(alias = "songmid")]
    mid: String,
    #[serde(alias = "songname")]
    title: String,
    #[serde(default)]
    singer: Vec<QqSinger>,
    #[serde(default)]
    album: QqAlbum,
    #[serde(default, alias = "interval")]
    interval: u64,
    #[serde(default)]
    file: QqFile,
    // Legacy `client_search_cp` items expose quality as flat size fields.
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size128: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size320: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    sizeape: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    sizeflac: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    sizeogg: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct QqFile {
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_128mp3: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_192aac: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_192ogg: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_96aac: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_96ogg: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_24aac: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_320mp3: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_ape: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_dolby: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_flac: u64,
    #[serde(default, deserialize_with = "crate::domain::de_u64_loose")]
    size_hires: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct QqSinger {
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct QqAlbum {
    #[serde(default, alias = "albumname")]
    title: String,
    #[serde(default, alias = "albummid")]
    mid: String,
}

#[derive(Debug, Deserialize)]
struct QqLyricEnvelope {
    lyric: Option<String>,
}

fn decode_base64(input: &str) -> Option<String> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let values = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .map(value)
        .collect::<Option<Vec<_>>>()?;
    let mut output = Vec::with_capacity(values.len() * 3 / 4);
    for chunk in values.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        output.push((chunk[0] << 2) | (chunk[1] >> 4));
        if chunk.len() > 2 {
            output.push((chunk[1] << 4) | (chunk[2] >> 2));
        }
        if chunk.len() > 3 {
            output.push((chunk[2] << 6) | chunk[3]);
        }
    }
    String::from_utf8(output).ok()
}

#[async_trait]
impl MusicSource for QqSource {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            id: "QQMusicClient",
            name: "QQ音乐",
            capabilities: &["search", "stream", "download", "lyrics"],
            enabled: true,
        }
    }

    async fn search(&self, query: &str, limit: usize, page: u32) -> Result<Vec<Track>, String> {
        let payload = json!({
            "comm": Self::common(),
            "music.search.SearchCgiService.DoSearchForQQMusicMobile": {
                "module": "music.search.SearchCgiService", "method": "DoSearchForQQMusicMobile",
                "param": {"searchid": Self::guid(), "query": query, "search_type": 0, "num_per_page": limit, "page_num": page, "highlight": 1, "grp": 1}
            }
        });
        let response = self
            .client
            .post(QQ_API)
            .header("User-Agent", QQ_UA)
            .header("Referer", "https://y.qq.com/")
            .header("Origin", "https://y.qq.com/")
            .json(&payload)
            .send()
            .await
            .map_err(|error| format!("QQ 音乐搜索请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("QQ 音乐搜索返回错误: {error}"))?;
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| format!("QQ 音乐搜索数据解析失败: {error}"))?;
        let songs = body
            .pointer("/music.search.SearchCgiService.DoSearchForQQMusicMobile/data/body/item_song")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let songs = if songs.is_empty() {
            self.legacy_search(query, limit, page).await?
        } else {
            songs
        };
        let parsed = songs
            .into_iter()
            .filter_map(|song| serde_json::from_value::<QqSong>(song).ok())
            .take(limit)
            .collect::<Vec<_>>();
        Ok(parsed.into_iter().map(Self::track_from_song).collect())
    }

    async fn resolve_track(&self, track: &Track) -> Result<Track, String> {
        if !track.audio_url.trim().is_empty() {
            return Ok(track.clone());
        }
        let song_mid = track
            .id
            .strip_prefix("qq:")
            .ok_or_else(|| "歌曲标识不属于 QQ 音乐".to_string())?;
        let (audio_url, quality, extension) = self.resolve_stream(song_mid).await;
        if audio_url.is_empty() {
            return Err("QQ 音乐未返回可用音频地址".into());
        }
        let mut resolved = track.clone();
        resolved.audio_url = audio_url;
        resolved.format = Some(extension.to_uppercase());
        resolved.quality = (!quality.is_empty()).then_some(quality);
        if let Some(payload) = resolved.adapter_payload.as_mut() {
            if let Some(object) = payload.as_object_mut() {
                object.insert("extension".into(), json!(extension));
            }
        }
        Ok(resolved)
    }

    async fn lyrics(&self, track: &Track) -> Result<Option<String>, String> {
        let song_mid = track
            .id
            .strip_prefix("qq:")
            .ok_or_else(|| "歌曲标识不属于 QQ 音乐".to_string())?;
        Ok(self.query_lyric(song_mid).await)
    }

    async fn resolve_quality(&self, track: &Track) -> Option<(String, String)> {
        let song_mid = track.id.strip_prefix("qq:")?;
        // Match what playback will actually resolve: the highest streamable tier
        // across the whole fallback chain, so the search card and playback agree.
        let (_, quality, extension) = self.resolve_stream(song_mid).await;
        if quality.is_empty() {
            return None;
        }
        Some((quality, extension.to_uppercase()))
    }
}

#[cfg(test)]
mod tests {
    use super::{qq_quality, QqAlbum, QqFile, QqSong};

    fn song(file: QqFile) -> QqSong {
        QqSong {
            mid: "m".into(),
            title: "t".into(),
            singer: vec![],
            album: QqAlbum::default(),
            interval: 0,
            file,
            size128: 0,
            size320: 0,
            sizeape: 0,
            sizeflac: 0,
            sizeogg: 0,
        }
    }

    // Legacy `client_search_cp` items carry flat size fields instead of `file`.
    fn legacy(size128: u64, size320: u64, sizeflac: u64, sizeogg: u64) -> QqSong {
        QqSong {
            size128,
            size320,
            sizeflac,
            sizeogg,
            ..song(QqFile::default())
        }
    }

    #[test]
    fn qq_prefers_lossless_then_standard() {
        // A lossless-capable card plays as 无损 (resolve tries lossless first).
        let s = song(QqFile { size_flac: 100, size_320mp3: 99, ..Default::default() });
        assert_eq!(qq_quality(&s), (Some("无损".into()), Some("FLAC".into())));

        let s = song(QqFile { size_320mp3: 100, size_128mp3: 99, ..Default::default() });
        assert_eq!(qq_quality(&s), (Some("320K".into()), Some("MP3".into())));

        let s = song(QqFile { size_128mp3: 100, size_dolby: 100, ..Default::default() });
        assert_eq!(qq_quality(&s), (Some("128K".into()), Some("MP3".into())));

        // Only VIP Dolby present and no standard tier -> nothing to report.
        let s = song(QqFile { size_dolby: 100, ..Default::default() });
        assert_eq!(qq_quality(&s), (None, None));

        assert_eq!(qq_quality(&song(QqFile::default())), (None, None));
    }

    #[test]
    fn qq_legacy_fields_report_quality() {
        // Legacy cards have no `file`; quality comes from flat size fields.
        let s = legacy(4_288_455, 10_720_847, 31_386_542, 6_600_445);
        assert_eq!(qq_quality(&s), (Some("无损".into()), Some("FLAC".into())));

        let s = legacy(4_288_455, 10_720_847, 0, 0);
        assert_eq!(qq_quality(&s), (Some("320K".into()), Some("MP3".into())));

        let s = legacy(4_288_455, 0, 0, 0);
        assert_eq!(qq_quality(&s), (Some("128K".into()), Some("MP3".into())));
    }

    #[test]
    fn qq_deserializes_real_file_sizes() {
        let song: QqSong = serde_json::from_value(serde_json::json!({
            "mid": "003Qui1q2u1Zho",
            "title": "晴天",
            "singer": [{"name": "周杰伦"}],
            "album": {"mid": "000MkMni19ClKG", "title": "叶惠美"},
            "interval": 269,
            "file": {"size_128mp3": 4317292, "size_320mp3": 10792943, "size_flac": 55397039, "size_dolby": 0}
        }))
        .unwrap();
        assert_eq!(song.file.size_flac, 55_397_039);
        assert_eq!(qq_quality(&song), (Some("无损".into()), Some("FLAC".into())));
    }
}

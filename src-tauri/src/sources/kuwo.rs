use super::MusicSource;
use crate::domain::{SourceDescriptor, Track};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

const KUWO_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

pub struct KuwoSource {
    client: reqwest::Client,
}

struct ResolvedAudio {
    url: String,
    quality: String,
    extension: String,
}

impl KuwoSource {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    // This follows musicdl's AudioLinkTester instead of accepting any 2xx URL:
    // follow redirects, sample bytes, and infer a real audio container.
    async fn inspect_audio(&self, url: &str) -> Option<(String, String)> {
        let mut response = self
            .client
            .get(url)
            .header("User-Agent", KUWO_UA)
            .header("Range", "bytes=0-8191")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("text/")
            || content_type.contains("json")
            || content_type.contains("html")
        {
            return None;
        }
        // Reading a chunk is essential: a number of relay URLs return a 2xx
        // header then fail to decode their actual response body.
        let sample = response.chunk().await.ok()??;
        if sample.is_empty() {
            return None;
        }
        let extension = if content_type.contains("flac") || sample.starts_with(b"fLaC") {
            "flac".to_owned()
        } else if content_type.contains("ogg") || sample.starts_with(b"OggS") {
            "ogg".to_owned()
        } else if content_type.contains("wav") || sample.starts_with(b"RIFF") {
            "wav".to_owned()
        } else if content_type.contains("mp4") || sample.get(4..8) == Some(b"ftyp") {
            "m4a".to_owned()
        } else if content_type.contains("mpeg")
            || sample.starts_with(b"ID3")
            || (sample.first() == Some(&0xff)
                && sample.get(1).is_some_and(|byte| byte & 0xe0 == 0xe0))
        {
            "mp3".to_owned()
        } else {
            final_url
                .split('?')
                .next()
                .and_then(|value| value.rsplit('.').next())
                .map(|value| value.to_ascii_lowercase())
                .filter(|value| {
                    matches!(
                        value.as_str(),
                        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav"
                    )
                })?
        };
        Some((final_url, extension))
    }

    async fn resolved_candidate(&self, url: &str, quality: &str) -> Option<ResolvedAudio> {
        let (url, extension) = self.inspect_audio(url).await?;
        Some(ResolvedAudio {
            url,
            quality: quality.into(),
            extension,
        })
    }

    async fn audio_url(&self, song_id: &str) -> Option<ResolvedAudio> {
        // Same enabled sequence as musicdl: cgg -> lxmusic -> nxinxz -> haitangw.
        if let Ok(response) = self
            .client
            .get("https://kw-api.cenguigui.cn/")
            .query(&[
                ("id", song_id),
                ("type", "song"),
                ("level", "lossless"),
                ("format", "json"),
            ])
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36")
            .send()
            .await
        {
            if let Ok(response) = response.error_for_status() {
                if let Ok(payload) = response.json::<KuwoAudioEnvelope>().await {
                    if let Some(url) = payload.data.and_then(|data| data.url).filter(|url| url.starts_with("http")) {
                        if let Some(audio) = self.resolved_candidate(&url, "无损").await {
                            return Some(audio);
                        }
                    }
                }
            }
        }

        if let Ok(response) = self
            .client
            .get(format!(
                "https://lxmusicapi.onrender.com/url/kw/{song_id}/flac"
            ))
            .header("Content-Type", "application/json")
            .header("User-Agent", "lx-music-request/2.6.0")
            .header("X-Request-Key", "share-v3")
            .send()
            .await
        {
            if let Ok(response) = response.error_for_status() {
                if let Ok(payload) = response.json::<Value>().await {
                    if let Some(url) = payload
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|url| url.starts_with("http"))
                    {
                        if let Some(audio) = self.resolved_candidate(url, "无损").await {
                            return Some(audio);
                        }
                    }
                }
            }
        }

        for base in [
            "http://music.nxinxz.com/kw.php",
            "https://musicapi.haitangw.net/music/kw.php",
        ] {
            for (level, quality) in [
                ("lossless", "无损"),
                ("exhigh", "320K"),
                ("standard", "128K"),
            ] {
                let result = self
                    .client
                    .get(base)
                    .query(&[("id", song_id), ("level", level), ("type", "json")])
                    .header("User-Agent", KUWO_UA)
                    .send()
                    .await;
                if let Ok(response) = result {
                    if let Ok(response) = response.error_for_status() {
                        if let Ok(value) = response.json::<Value>().await {
                            if let Some(url) = value
                                .pointer("/data/url")
                                .and_then(Value::as_str)
                                .filter(|url| url.starts_with("http"))
                            {
                                if let Some(audio) = self.resolved_candidate(url, quality).await {
                                    return Some(audio);
                                }
                            }
                        }
                    }
                }
            }
        }

        // The original KuwoMusicClient also keeps the NOBB L2 parser after
        // the cgg/lxmusic/nxinxz/haitangw chain. Do not return an unverified
        // link before this last source-backed fallback has been attempted.
        if let Ok(response) = self
            .client
            .get("https://api.nobb.cc/kuwo.music/index.php")
            .query(&[("id", song_id)])
            .header("User-Agent", KUWO_UA)
            .send()
            .await
        {
            if let Ok(response) = response.error_for_status() {
                if let Ok(value) = response.json::<Value>().await {
                    if let Some(url) = value
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|url| url.starts_with("http"))
                    {
                        if let Some(audio) = self.resolved_candidate(url, "").await {
                            return Some(audio);
                        }
                    }
                }
            }
        }
        None
    }

    fn track_from_value(song: Value) -> Option<Track> {
        let music_rid = song
            .get("MUSICRID")
            .or_else(|| song.get("musicrid"))
            .and_then(Value::as_str)?
            .to_string();
        let song_name = song
            .get("SONGNAME")
            .or_else(|| song.get("NAME"))
            .or_else(|| song.get("songName"))
            .or_else(|| song.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let artist = song
            .get("ARTIST")
            .or_else(|| song.get("artist"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let album = song
            .get("ALBUM")
            .or_else(|| song.get("album"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let duration = song
            .get("DURATION")
            .or_else(|| song.get("duration"))
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_string();
        let artwork = song
            .get("hts_MVPIC")
            .or_else(|| song.get("albumpic"))
            .or_else(|| song.get("pic"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let song_id = music_rid.trim_start_matches("MUSIC_").to_string();
        Some(Track {
            id: format!("kuwo:{song_id}"),
            source: "KuwoMusicClient".into(),
            title: song_name,
            artist,
            album,
            artwork_url: artwork,
            audio_url: String::new(),
            duration_ms: duration.parse::<u64>().unwrap_or_default() * 1000,
            format: None,
            quality: None,
            adapter_payload: Some(json!({"downloadHeaders": {"User-Agent": KUWO_UA} })),
        })
    }
}

#[derive(Debug, Deserialize)]
struct KuwoAudioEnvelope {
    data: Option<KuwoAudioData>,
}
#[derive(Debug, Deserialize)]
struct KuwoAudioData {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KuwoLyricEnvelope {
    data: Option<KuwoLyricData>,
}

#[derive(Debug, Deserialize)]
struct KuwoLyricData {
    #[serde(default)]
    lrclist: Vec<KuwoLyricLine>,
}

#[derive(Debug, Deserialize)]
struct KuwoLyricLine {
    #[serde(rename = "lineLyric")]
    line_lyric: String,
    time: String,
}

#[async_trait]
impl MusicSource for KuwoSource {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            id: "KuwoMusicClient",
            name: "酷我音乐",
            capabilities: &["search", "stream", "download", "lyrics"],
            enabled: true,
        }
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>, String> {
        let response = self
            .client
            .get("https://www.kuwo.cn/search/searchMusicBykeyWord")
            .query(&[
                ("vipver", "1"),
                ("client", "kt"),
                ("ft", "music"),
                ("cluster", "0"),
                ("strategy", "2012"),
                ("encoding", "utf8"),
                ("rformat", "json"),
                ("mobi", "1"),
                ("issubtitle", "1"),
                ("show_copyright_off", "1"),
                ("pn", "0"),
                ("rn", &limit.to_string()),
                ("all", query),
            ])
            .header("User-Agent", KUWO_UA)
            .header("Referer", "https://www.kuwo.cn/")
            .header("Origin", "https://www.kuwo.cn")
            .send()
            .await
            .map_err(|error| format!("酷我音乐搜索请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("酷我音乐搜索返回错误: {error}"))?;
        let raw = response
            .text()
            .await
            .map_err(|error| format!("酷我音乐搜索响应读取失败: {error}"))?;
        let payload = raw.trim();
        let payload = payload
            .strip_prefix("callback(")
            .and_then(|value| value.strip_suffix(')'))
            .unwrap_or(payload);
        let response = serde_json::from_str::<Value>(payload).map_err(|error| {
            format!(
                "酷我音乐搜索数据解析失败: {error}; 响应: {}",
                payload.chars().take(120).collect::<String>()
            )
        })?;

        // Kuwo can repeat object keys such as `SONGNAME`.  Parsing as `Value`
        // intentionally follows the source's last-value JSON behaviour instead
        // of rejecting an otherwise valid search page.
        let songs = response
            .get("abslist")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs
            .into_iter()
            .take(limit)
            .filter_map(Self::track_from_value)
            .collect())
    }

    async fn resolve_track(&self, track: &Track) -> Result<Track, String> {
        if !track.audio_url.trim().is_empty() {
            return Ok(track.clone());
        }
        let song_id = track
            .id
            .strip_prefix("kuwo:")
            .ok_or_else(|| "歌曲标识不属于酷我音乐".to_string())?;
        let resolved_audio = self
            .audio_url(song_id)
            .await
            .ok_or_else(|| "酷我音乐未返回可用音频地址".to_string())?;
        let mut resolved = track.clone();
        resolved.audio_url = resolved_audio.url;
        resolved.format = Some(resolved_audio.extension.to_uppercase());
        resolved.quality = (!resolved_audio.quality.is_empty()).then_some(resolved_audio.quality);
        Ok(resolved)
    }

    async fn lyrics(&self, track: &Track) -> Result<Option<String>, String> {
        let song_id = track
            .id
            .strip_prefix("kuwo:")
            .ok_or_else(|| "歌曲标识不属于酷我音乐".to_string())?;
        let response = self
            .client
            .get("https://kuwo.cn/openapi/v1/www/lyric/getlyric")
            .query(&[("musicId", song_id)])
            .header("Referer", "https://www.kuwo.cn/")
            .send()
            .await
            .map_err(|error| format!("酷我歌词请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("酷我歌词接口返回错误: {error}"))?
            .json::<KuwoLyricEnvelope>()
            .await
            .map_err(|error| format!("酷我歌词数据解析失败: {error}"))?;
        let lines = response.data.map(|data| data.lrclist).unwrap_or_default();
        if lines.is_empty() {
            return Ok(None);
        }
        let lyric = lines
            .into_iter()
            .filter_map(|line| {
                let seconds = line.time.parse::<f64>().ok()?;
                let minutes = (seconds / 60.0).floor() as u64;
                let remainder = seconds - minutes as f64 * 60.0;
                Some(format!(
                    "[{minutes:02}:{remainder:05.2}]{}",
                    line.line_lyric
                ))
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok((!lyric.is_empty()).then_some(lyric))
    }
}

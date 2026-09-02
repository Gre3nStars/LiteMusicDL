use super::MusicSource;
use crate::domain::{SourceDescriptor, Track};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

const NETEASE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const NETEASE_REFERER: &str = "https://music.163.com/";

fn chrono_like_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Higher number = better quality, used to pick the best URL among resolvers
/// that all succeeded in parallel. Mirrors musicdl's MUSIC_QUALITIES ordering.
fn level_rank(level: &str) -> u8 {
    match level {
        "jymaster" => 8,
        "jyeffect" | "sky" => 7,
        "hires" => 6,
        "lossless" => 5,
        "dolby" => 4,
        "exhigh" => 3,
        "standard" => 2,
        _ => 1,
    }
}

pub struct NeteaseSource {
    client: reqwest::Client,
}

impl NeteaseSource {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// musicdl's NeteaseClient tries a *long* list of third-party parsers
    /// (`_parsewiththirdpartapis`) so a single dead/rate-limited API does not
    /// fail playback. We keep that spirit but ordered by observed reliability
    /// and quality: toubiec (official-grade, full tier) first, then independent
    /// lossless FLAC resolvers, then a plain MP3 last resort. All resolvers run
    /// concurrently so a slow parser (toubiec needs an `ip` pre-flight plus 8
    /// tiers) never delays the fast backend-account resolvers; quality tiers are
    /// then ranked so the highest available wins.
    async fn audio_url(&self, id: u64) -> (String, String) {
        use futures_util::future::{join_all, BoxFuture};
        // Highest-quality tier first; a lossless FLAC beats a plain MP3.
        let resolvers: Vec<BoxFuture<'_, Option<(String, String)>>> = vec![
            Box::pin(self.toubiec_url(id)),
            Box::pin(self.gdstudio_url(id)),
            Box::pin(self.cocodownloader_url(id)),
            Box::pin(self.haitangw_url(id)),
            Box::pin(self.ffapi_url(id)),
        ];
        let mut candidates = join_all(resolvers)
            .await
            .into_iter()
            .filter_map(|result| result)
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| level_rank(&b.1).cmp(&level_rank(&a.1)));
        for (url, level) in candidates {
            if !url.is_empty() && self.playable(&url).await {
                return (url, level);
            }
        }
        (String::new(), String::new())
    }

    /// Confirm the URL actually returns audio bytes before we hand it to the
    /// player/downloader. Some APIs surface a "successful" URL that still 404s
    /// or is a risk-control HTML page; a ranged HEAD-style probe keeps playback
    /// from failing on a dead link.
    async fn playable(&self, url: &str) -> bool {
        self.client
            .get(url)
            .timeout(Duration::from_secs(6))
            .header("User-Agent", NETEASE_UA)
            .header("Referer", NETEASE_REFERER)
            .send()
            .await
            .map(|response| {
                let status = response.status();
                let kind = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                let is_audio = kind.starts_with("audio/")
                    || kind.contains("application/octet-stream")
                    || kind.contains("binary")
                    || kind.is_empty();
                (status.is_success() || status.as_u16() == 206) && is_audio
            })
            .unwrap_or(false)
    }

    /// `nextmusic.toubiec.cn` — the same parser musicdl uses as `_parsewithxiaoqinapi`.
    /// Needs an `ip` token from `/api/ip`, then each tier is requested with `ip`.
    async fn toubiec_url(&self, id: u64) -> Option<(String, String)> {
        let ip = self.parse_ip().await?;
        for level in [
            "jymaster", "jyeffect", "sky", "hires", "lossless", "dolby", "exhigh", "standard",
        ] {
            let payload = json!({
                "id": id.to_string(),
                "level": level,
                "timestamp": chrono_like_timestamp(),
                "ip": ip,
            });
            let url = match self.toubiec_endpoint("getSongUrl", &payload).await {
                Some(url) => Some(url),
                None => self.toubiec_endpoint("getMusicUrl", &payload).await,
            };
            if let Some(url) = url {
                return Some((url, level.to_string()));
            }
        }
        None
    }

    async fn parse_ip(&self) -> Option<String> {
        self.client
            .post("https://nextmusic.toubiec.cn/api/ip")
            .timeout(Duration::from_secs(8))
            .header("Accept", "*/*")
            .header("Origin", "https://wyapi.toubiec.cn")
            .header("Referer", "https://wyapi.toubiec.cn/")
            .header("User-Agent", NETEASE_UA)
            .json(&json!({"timestamp": chrono_like_timestamp()}))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?
            .pointer("/data/ip")?
            .as_str()
            .map(str::to_string)
    }

    async fn toubiec_endpoint(&self, api: &str, payload: &serde_json::Value) -> Option<String> {
        let value = self
            .client
            .post(format!("https://nextmusic.toubiec.cn/api/{api}"))
            .timeout(Duration::from_secs(8))
            .header("Accept", "*/*")
            .header("Origin", "https://wyapi.toubiec.cn")
            .header("Referer", "https://wyapi.toubiec.cn/")
            .header("User-Agent", NETEASE_UA)
            .json(payload)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;
        let url = value.pointer("/data/url")?.as_str()?;
        if url.starts_with("http") {
            Some(url.to_string())
        } else {
            None
        }
    }

    /// `music-api.gdstudio.xyz` — simple GET, returns real lossless FLAC for free
    /// songs. Reliable, no key.
    async fn gdstudio_url(&self, id: u64) -> Option<(String, String)> {
        let value = self
            .client
            .get("https://music-api.gdstudio.xyz/api.php")
            .timeout(Duration::from_secs(8))
            .query(&[
                ("types", "url".to_string()),
                ("id", id.to_string()),
                ("source", "netease".to_string()),
                ("br", "999".to_string()),
            ])
            .header("User-Agent", NETEASE_UA)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;
        let url = value.get("url")?.as_str()?;
        if url.starts_with("http") {
            Some((url.to_string(), "lossless".to_string()))
        } else {
            None
        }
    }

    /// `cocodownloader.markqq.com` — another keyless lossless FLAC resolver.
    async fn cocodownloader_url(&self, id: u64) -> Option<(String, String)> {
        let value = self
            .client
            .get("https://cocodownloader.markqq.com/api/url")
            .timeout(Duration::from_secs(8))
            .query(&[
                ("id", id.to_string()),
                ("provider", "netease".to_string()),
                ("quality", "lossless".to_string()),
            ])
            .header("User-Agent", NETEASE_UA)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;
        let url = value.get("url")?.as_str()?;
        if url.starts_with("http") {
            Some((url.to_string(), "lossless".to_string()))
        } else {
            None
        }
    }

    /// `musicapi.haitangw.net` — multi-tier (lossless/hires/exhigh/standard).
    async fn haitangw_url(&self, id: u64) -> Option<(String, String)> {
        for level in ["lossless", "hires", "exhigh", "standard"] {
            let value = self
                .client
                .get("https://musicapi.haitangw.net/music/wy.php")
                .timeout(Duration::from_secs(8))
                .query(&[
                    ("id", id.to_string()),
                    ("level", level.to_string()),
                    ("type", "json".to_string()),
                ])
                .header("User-Agent", NETEASE_UA)
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json::<serde_json::Value>()
                .await
                .ok()?;
            let url = value.pointer("/data/url")?.as_str()?;
            if url.starts_with("http") {
                return Some((url.to_string(), level.to_string()));
            }
        }
        None
    }

    /// `ffapi.cn` — plain MP3 (128k) last resort; a bit slower but keyless.
    async fn ffapi_url(&self, id: u64) -> Option<(String, String)> {
        let value = self
            .client
            .get("https://ffapi.cn/int/v1/netease_url")
            .timeout(Duration::from_secs(8))
            .query(&[("id", &id.to_string())])
            .header("User-Agent", NETEASE_UA)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;
        let url = value.get("url")?.as_str()?;
        if url.starts_with("http") {
            Some((url.to_string(), "standard".to_string()))
        } else {
            None
        }
    }

    fn track_from_song(song: NeteaseSong) -> Track {
        let artist = song
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let (quality, format) = netease_quality(&song);
        let album_name = song
            .album
            .as_ref()
            .map(|album| album.name.clone())
            .unwrap_or_default();
        // Netease reports album art over `http://`; force `https://` so the
        // Tauri/CSP webview does not drop the cover as mixed content.
        let artwork_url = song
            .album
            .and_then(|album| album.pic_url)
            .map(|url| if let Some(rest) = url.strip_prefix("http://") {
                format!("https://{rest}")
            } else {
                url
            })
            .unwrap_or_default();
        Track {
            id: format!("netease:{}", song.id),
            source: "NeteaseMusicClient".into(),
            title: song.name,
            artist,
            album: album_name,
            artwork_url,
            audio_url: String::new(),
            duration_ms: song.duration.unwrap_or_default(),
            format,
            quality,
            adapter_payload: None,
        }
    }
}

/// The quality reported by the search card, from `privilege.maxBrLevel` (the
/// tier the toubiec resolver can actually reach), falling back to `sq`/`h`/`m`/`l`.
fn netease_quality(song: &NeteaseSong) -> (Option<String>, Option<String>) {
    if let Some(privilege) = song.privilege.as_ref() {
        if let Some(level) = privilege.max_br_level.as_deref() {
            let level = level.to_ascii_lowercase();
            if level.contains("dolby") {
                return (Some("杜比全景声".into()), Some("FLAC".into()));
            }
            if level.contains("jymaster") {
                return (Some("臻品母带".into()), Some("FLAC".into()));
            }
            if level.contains("jyeffect") || level.contains("sky") {
                return (Some("臻品音效".into()), Some("FLAC".into()));
            }
            if level.contains("lossless") || level.contains("hires") {
                return (Some("无损".into()), Some("FLAC".into()));
            }
            if level.contains("exhigh") {
                return (Some("320K".into()), Some("MP3".into()));
            }
        }
    }
    if song.sq.is_some() {
        (Some("无损".into()), Some("FLAC".into()))
    } else if song.h.is_some() {
        (Some("320K".into()), Some("MP3".into()))
    } else if song.m.is_some() {
        (Some("192K".into()), Some("MP3".into()))
    } else if song.l.is_some() {
        (Some("128K".into()), Some("MP3".into()))
    } else {
        (None, None)
    }
}

#[derive(Debug, Deserialize)]
struct SearchEnvelope {
    result: Option<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    songs: Option<Vec<NeteaseSong>>,
}

#[derive(Debug, Deserialize)]
struct NeteaseSong {
    id: u64,
    name: String,
    #[serde(default, alias = "ar")]
    artists: Vec<NeteaseArtist>,
    #[serde(alias = "al")]
    album: Option<NeteaseAlbum>,
    #[serde(alias = "dt")]
    duration: Option<u64>,
    #[serde(default)]
    privilege: Option<NeteasePrivilege>,
    #[serde(default)]
    sq: Option<serde_json::Value>,
    #[serde(default)]
    h: Option<serde_json::Value>,
    #[serde(default)]
    m: Option<serde_json::Value>,
    #[serde(default)]
    l: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct NeteasePrivilege {
    #[serde(rename = "maxBrLevel")]
    max_br_level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NeteaseArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct NeteaseAlbum {
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LyricEnvelope {
    lrc: Option<LyricBody>,
}

#[derive(Debug, Deserialize)]
struct LyricBody {
    lyric: Option<String>,
}

#[async_trait]
impl MusicSource for NeteaseSource {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            id: "NeteaseMusicClient",
            name: "网易云音乐",
            capabilities: &["search", "stream", "download", "lyrics"],
            enabled: true,
        }
    }

    async fn search(&self, query: &str, limit: usize, page: u32) -> Result<Vec<Track>, String> {
        let offset = (page.saturating_sub(1) as usize).saturating_mul(limit);
        let response = self
            .client
            .post("https://music.163.com/api/cloudsearch/pc")
            .form(&[
                ("s", query.to_owned()),
                ("type", "1".into()),
                ("offset", offset.to_string()),
                ("total", "true".into()),
                ("limit", limit.to_string()),
            ])
            .header("Referer", "https://music.163.com/")
            .send()
            .await
            .map_err(|error| format!("网易云搜索请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("网易云搜索返回错误: {error}"))?
            .json::<SearchEnvelope>()
            .await
            .map_err(|error| format!("网易云搜索数据解析失败: {error}"))?;

        Ok(response
            .result
            .and_then(|result| result.songs)
            .unwrap_or_default()
            .into_iter()
            .map(Self::track_from_song)
            .collect())
    }

    async fn resolve_track(&self, track: &Track) -> Result<Track, String> {
        if !track.audio_url.trim().is_empty() {
            return Ok(track.clone());
        }
        let id = track
            .id
            .strip_prefix("netease:")
            .ok_or_else(|| "歌曲标识不属于网易云音乐".to_string())?
            .parse::<u64>()
            .map_err(|_| "网易云音乐歌曲标识无效".to_string())?;
        let (audio_url, quality) = self.audio_url(id).await;
        if audio_url.is_empty() {
            return Err(
                "网易云音乐未能解析出可播放的音频地址（该歌曲可能为付费/版权曲目，或当前音解析服务暂时不可用）"
                    .into(),
            );
        }
        let extension = audio_url
            .split('?')
            .next()
            .and_then(|url| url.rsplit('.').next())
            .filter(|ext| ext.len() <= 5)
            .unwrap_or("mp3")
            .to_uppercase();
        let extension_lower = extension.to_ascii_lowercase();
        let mut resolved = track.clone();
        resolved.audio_url = audio_url;
        resolved.format = Some(extension);
        resolved.adapter_payload = Some(json!({
            "extension": extension_lower,
            "downloadHeaders": {"Referer": NETEASE_REFERER, "User-Agent": NETEASE_UA}
        }));
        resolved.quality = Some(
            match quality.as_str() {
                "jymaster" => "臻品母带",
                "jyeffect" => "臻品音效",
                "sky" => "沉浸环绕声",
                "lossless" | "hires" => "无损",
                "dolby" => "杜比全景声",
                "exhigh" => "320K",
                "standard" => "128K",
                _ => quality.as_str(),
            }
            .to_string(),
        );
        Ok(resolved)
    }

    async fn lyrics(&self, track: &Track) -> Result<Option<String>, String> {
        let song_id = track
            .id
            .strip_prefix("netease:")
            .ok_or_else(|| "歌曲标识不属于网易云音乐".to_string())?;
        let response = self
            .client
            .get("https://music.163.com/api/song/lyric")
            .query(&[("id", song_id), ("lv", "-1"), ("kv", "-1"), ("tv", "-1")])
            .header("Referer", NETEASE_REFERER)
            .send()
            .await
            .map_err(|error| format!("歌词请求失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("歌词接口返回错误: {error}"))?
            .json::<LyricEnvelope>()
            .await
            .map_err(|error| format!("歌词数据解析失败: {error}"))?;
        Ok(response
            .lrc
            .and_then(|body| body.lyric)
            .filter(|lyric| !lyric.trim().is_empty()))
    }

    async fn resolve_quality(&self, track: &Track) -> Option<(String, String)> {
        let id = track.id.strip_prefix("netease:")?.parse::<u64>().ok()?;
        let (url, level) = self.audio_url(id).await;
        if url.is_empty() {
            return None;
        }
        let extension = url
            .split('?')
            .next()
            .and_then(|url| url.rsplit('.').next())
            .filter(|ext| ext.len() <= 5)
            .unwrap_or("mp3")
            .to_uppercase();
        let label = match level.as_str() {
            "jymaster" => "臻品母带",
            "jyeffect" => "臻品音效",
            "sky" => "沉浸环绕声",
            "lossless" | "hires" => "无损",
            "dolby" => "杜比全景声",
            "exhigh" => "320K",
            "standard" => "128K",
            other => other,
        };
        Some((label.to_string(), extension))
    }
}

#[cfg(test)]
mod tests {
    use super::{netease_quality, NeteasePrivilege, NeteaseSong};

    #[test]
    fn netease_uses_max_br_level() {
        // toubiec can deliver up to maxBrLevel, so a lossless-capable card plays
        // as 无损; standard tiers map to 320K/128K.
        let song = NeteaseSong {
            id: 1,
            name: "song".into(),
            artists: vec![],
            album: None,
            duration: Some(1000),
            privilege: Some(NeteasePrivilege { max_br_level: Some("lossless".into()) }),
            sq: None,
            h: None,
            m: None,
            l: None,
        };
        assert_eq!(netease_quality(&song), (Some("无损".into()), Some("FLAC".into())));

        let song = NeteaseSong { privilege: Some(NeteasePrivilege { max_br_level: Some("exhigh".into()) }), ..song };
        assert_eq!(netease_quality(&song), (Some("320K".into()), Some("MP3".into())));

        let song = NeteaseSong { privilege: None, h: Some(serde_json::json!({})), ..song };
        assert_eq!(netease_quality(&song), (Some("320K".into()), Some("MP3".into())));
    }

    #[test]
    fn netease_artwork_url_is_https() {
        use crate::sources::netease::NeteaseAlbum;
        let song = NeteaseSong {
            id: 1,
            name: "song".into(),
            artists: vec![],
            album: Some(NeteaseAlbum { name: "album".into(), pic_url: Some("http://p2.music.126.net/abc/cover.jpg".into()) }),
            duration: Some(1000),
            privilege: None,
            sq: None,
            h: None,
            m: None,
            l: None,
        };
        let track = super::NeteaseSource::track_from_song(song);
        assert_eq!(track.artwork_url, "https://p2.music.126.net/abc/cover.jpg");
    }
}

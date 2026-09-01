use super::MusicSource;
use crate::domain::{SourceDescriptor, Track};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct NeteaseSource {
    client: reqwest::Client,
}

impl NeteaseSource {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    async fn audio_url(&self, id: u64) -> (String, String) {
        // `_parsewithxiaoqinapi` in musicdl uses these exact quality keys and
        // retries the companion endpoint when it did not honour the level.
        for level in [
            "jymaster", "jyeffect", "sky", "hires", "lossless", "dolby", "exhigh", "standard",
        ] {
            let payload =
                json!({"id": id.to_string(), "level": level, "timestamp": chrono_like_timestamp()});
            let mut value = self.netease_parser_request("getSongUrl", &payload).await;
            if value
                .as_ref()
                .and_then(|value| value.pointer("/data/level"))
                .and_then(|value| value.as_str())
                != Some(level)
            {
                value = self.netease_parser_request("getMusicUrl", &payload).await;
            }
            if let Some(url) = value
                .as_ref()
                .and_then(|value| value.pointer("/data/url"))
                .and_then(|value| value.as_str())
                .filter(|value| value.starts_with("http"))
            {
                return (url.to_string(), level.to_string());
            }
        }
        (String::new(), String::new())
    }

    async fn netease_parser_request(
        &self,
        method: &str,
        payload: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.client.post(format!("https://nextmusic.toubiec.cn/api/{method}"))
            .header("Accept", "*/*").header("Origin", "https://wyapi.toubiec.cn").header("Referer", "https://wyapi.toubiec.cn/")
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36")
            .json(payload).send().await.ok()?.error_for_status().ok()?.json().await.ok()
    }

    fn track_from_song(song: NeteaseSong) -> Track {
        let artist = song
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        Track {
            id: format!("netease:{}", song.id),
            source: "NeteaseMusicClient".into(),
            title: song.name,
            artist,
            album: song
                .album
                .as_ref()
                .map(|album| album.name.clone())
                .unwrap_or_default(),
            artwork_url: song
                .album
                .and_then(|album| album.pic_url)
                .unwrap_or_default(),
            audio_url: String::new(),
            duration_ms: song.duration.unwrap_or_default(),
            format: None,
            quality: None,
            adapter_payload: None,
        }
    }
}

fn chrono_like_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>, String> {
        let response = self
            .client
            .post("https://music.163.com/api/cloudsearch/pc")
            .form(&[
                ("s", query.to_owned()),
                ("type", "1".into()),
                ("offset", "0".into()),
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
            return Err("网易云音乐未返回可用音频地址".into());
        }
        let extension = audio_url
            .split('?')
            .next()
            .and_then(|url| url.rsplit('.').next())
            .filter(|ext| ext.len() <= 5)
            .unwrap_or("mp3")
            .to_uppercase();
        let mut resolved = track.clone();
        resolved.audio_url = audio_url;
        resolved.format = Some(extension);
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
            .header("Referer", "https://music.163.com/")
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
}

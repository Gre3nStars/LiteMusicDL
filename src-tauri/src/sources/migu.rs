use super::MusicSource;
use crate::domain::{quality_from_url, SourceDescriptor, Track};
use async_trait::async_trait;
use futures_util::future::join_all;
use serde_json::{json, Value};

const MIGU_KEY: &[u8] = b"Jk8qzuePiJ1qE3mDYhLQ3T73DtDoAhLP";
const MIGU_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";

async fn decode_response(response: reqwest::Response) -> Result<Value, String> {
    let signed = response
        .headers()
        .get("signature")
        .and_then(|v| v.to_str().ok())
        == Some("1");
    let raw = response.bytes().await.map_err(|e| e.to_string())?;
    let plain = if signed || raw.starts_with(b"\xab\xcd\x01") {
        if raw.len() < 4 {
            return Err("咪咕响应过短".into());
        }
        let seed = raw[3];
        raw[4..]
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                byte.wrapping_add(seed)
                    .wrapping_sub(MIGU_KEY[i % MIGU_KEY.len()])
            })
            .collect::<Vec<_>>()
    } else {
        raw.to_vec()
    };
    serde_json::from_slice(&plain).map_err(|e| format!("咪咕数据解析失败: {e}"))
}

pub struct MiguSource {
    client: reqwest::Client,
}
impl MiguSource {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// musicdl does not take the duration from the search card. It reads
// `data.song.duration` from the successful listen-url response; search cards
// commonly omit that field, which was causing every row to render as 00:00.
fn duration_seconds(value: Option<&Value>) -> u64 {
    value
        .and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_f64().map(|number| number as u64))
                .or_else(|| {
                    item.as_str()
                        .and_then(|number| number.parse::<f64>().ok())
                        .map(|number| number as u64)
                })
        })
        .unwrap_or_default()
}

#[async_trait]
impl MusicSource for MiguSource {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            id: "MiguMusicClient",
            name: "咪咕音乐",
            capabilities: &["search", "stream", "download", "lyrics"],
            enabled: true,
        }
    }
    async fn search(&self, query: &str, limit: usize, page: u32) -> Result<Vec<Track>, String> {
        let response = self
            .client
            .get("https://c.musicapp.migu.cn/v1.0/content/search_all.do")
            .query(&[
                ("text", query),
                ("pageNo", &page.to_string()),
                ("pageSize", &limit.to_string()),
                ("isCopyright", "1"),
                ("sort", "1"),
                (
                    "searchSwitch",
                    r#"{"song":1,"album":0,"singer":0,"tagSong":1,"mvSong":0,"bestShow":1}"#,
                ),
            ])
            .header("User-Agent", MIGU_UA)
            .header("Accept", "application/json, text/plain, */*")
            .header("Origin", "https://h5.nf.migu.cn")
            .header("Referer", "https://h5.nf.migu.cn/")
            .header("ua", "Android_migu")
            .header("version", "6.8.8")
            .header("channel", "014021I")
            .header("subchannel", "014021I")
            .send()
            .await
            .map_err(|e| format!("咪咕搜索请求失败: {e}"))?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let body = decode_response(response).await?;
        let list = body
            .pointer("/songResultData/result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tasks = list.into_iter().take(limit).map(|song| {
            let source = self;
            async move {
                let id = text(&song, "contentId");
                let artist = song
                    .get("singers")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.get("name").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .unwrap_or_default();
                let album = text(&song, "albumName");
                let artwork = song
                    .get("imgItems")
                    .and_then(Value::as_array)
                    .and_then(|a| a.last())
                    .and_then(|v| v.get("img").or_else(|| v.get("url")))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let (audio, format, source_quality, duration) = if let (Some(copyright_id), Some(rates)) = (
                    song.get("copyrightId").and_then(Value::as_str),
                    song.get("rateFormats").and_then(Value::as_array),
                ) {
                    let mut candidates = rates
                        .iter()
                        .chain(
                            song.get("newRateFormats")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten(),
                        )
                        .chain(
                            song.get("audioFormats")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten(),
                        )
                        .filter(|v| v.get("formatType").and_then(Value::as_str) != Some("Z3D"))
                        .collect::<Vec<_>>();
                    candidates.sort_by(|a, b| migu_size(b).total_cmp(&migu_size(a)));
                    let mut resolved: (String, String, String, u64) =
                        (String::new(), "mp3".into(), String::new(), 0);
                    for candidate in candidates {
                        let (Some(tone), Some(resource)) = (candidate.get("formatType").and_then(Value::as_str), candidate.get("resourceType").and_then(Value::as_str)) else { continue };
                        let (url, response_duration) = match source.client.get("https://c.musicapp.migu.cn/strategy/listen-url/h5/v2.4")
                            .query(&[("contentId", id.as_str()), ("copyrightId", copyright_id), ("resourceType", resource), ("netType", "01"), ("toneFlag", tone), ("scene", ""), ("lowerQualityContentId", id.as_str())])
                            .header("User-Agent", MIGU_UA).header("Accept", "application/json, text/plain, */*")
                            .header("Origin", "https://h5.nf.migu.cn").header("Referer", "https://h5.nf.migu.cn/")
                            .header("ua", "Android_migu").header("version", "6.8.8").header("channel", "014021I").header("subchannel", "014021I")
                            .header("Content-Type", "application/json;charset=UTF-8").header("birth", "h5page").header("signature", "1").send().await {
                                Ok(response) => decode_response(response).await.ok().map(|value| (
                                    value.pointer("/data/url").and_then(Value::as_str).map(str::to_string).unwrap_or_default(),
                                    duration_seconds(value.pointer("/data/song/duration")),
                                )).unwrap_or_default(),
                                Err(_) => (String::new(), 0),
                            };
                        if !url.is_empty() {
                            let ext = match tone { "SQ" | "ZQ" | "ZQ24" | "ZQ32" => "flac", _ => "mp3" };
                            resolved = (url, ext.into(), tone.into(), response_duration);
                            break;
                        }
                    }
                    resolved
                } else {
                    (String::new(), "mp3".into(), String::new(), 0)
                };
                let quality = if source_quality.is_empty() {
                    quality_from_url(&audio)
                } else {
                    source_quality
                };
                Track {
                    id: format!("migu:{id}"),
                    source: "MiguMusicClient".into(),
                    title: text(&song, "name"),
                    artist,
                    album,
                    artwork_url: artwork,
                    audio_url: audio,
                    duration_ms: duration * 1000,
                    format: Some(format.to_uppercase()),
                    quality: Some(quality),
                    adapter_payload: Some(
                        json!({"lyricUrl": text(&song, "lyricUrl"), "extension": format, "downloadHeaders": {"User-Agent": MIGU_UA}}),
                    ),
                }
            }
        });
        Ok(join_all(tasks).await)
    }
    async fn lyrics(&self, track: &Track) -> Result<Option<String>, String> {
        let url = track
            .adapter_payload
            .as_ref()
            .and_then(|v| v.get("lyricUrl"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let Some(url) = url else { return Ok(None) };
        let value = match self
            .client
            .get(url)
            .header("User-Agent", MIGU_UA)
            .header("Referer", "https://y.migu.cn/")
            .send()
            .await
        {
            Ok(response) => response.text().await.ok(),
            Err(_) => None,
        };
        Ok(value.filter(|v| !v.trim().is_empty()))
    }
}

fn migu_size(value: &&Value) -> f64 {
    ["size", "iosSize", "androidSize", "isize", "asize"]
        .iter()
        .find_map(|key| value.get(*key))
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|value| value.trim_end_matches("MB").trim().parse().ok())
            })
        })
        .unwrap_or(0.0)
}

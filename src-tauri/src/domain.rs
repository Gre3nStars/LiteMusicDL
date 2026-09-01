use serde::{Deserialize, Serialize};

pub fn quality_from_url(url: &str) -> String {
    let value = url.to_ascii_lowercase();
    if value.contains("flac") || value.contains("lossless") {
        "无损".into()
    } else if value.contains("320") {
        "320K".into()
    } else if value.contains("128") || value.contains("120") {
        "128K".into()
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub source: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork_url: String,
    pub audio_url: String,
    pub duration_ms: u64,
    pub format: Option<String>,
    pub quality: Option<String>,
    pub adapter_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub capabilities: &'static [&'static str],
    pub enabled: bool,
}

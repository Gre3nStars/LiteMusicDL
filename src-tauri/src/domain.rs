use serde::{Deserialize, Serialize};

/// Deserialise a `u64` that a source may send as a JSON number OR a numeric
/// string (some APIs toggle between the two), so a string value never drops the
/// whole track during search parsing.
pub fn de_u64_loose<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Visitor;
    struct Loose;
    impl<'de> Visitor<'de> for Loose {
        type Value = u64;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an integer or a numeric string")
        }
        fn visit_u64<E>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }
        fn visit_i64<E>(self, value: i64) -> Result<u64, E> {
            Ok(value.max(0) as u64)
        }
        fn visit_str<E>(self, value: &str) -> Result<u64, E> {
            Ok(value.trim().parse::<u64>().unwrap_or(0))
        }
        fn visit_string<E>(self, value: String) -> Result<u64, E> {
            Ok(value.trim().parse::<u64>().unwrap_or(0))
        }
        fn visit_unit<E>(self) -> Result<u64, E> {
            Ok(0)
        }
    }
    deserializer.deserialize_any(Loose)
}

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

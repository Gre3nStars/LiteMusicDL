mod kuwo;
mod migu;
mod netease;
mod qq;

use crate::domain::{SourceDescriptor, Track};
use async_trait::async_trait;
use std::sync::Arc;

pub use kuwo::KuwoSource;
pub use migu::MiguSource;
pub use netease::NeteaseSource;
pub use qq::QqSource;

#[async_trait]
pub trait MusicSource: Send + Sync {
    fn descriptor(&self) -> SourceDescriptor;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>, String>;
    /// Search is deliberately metadata-first. Some source-specific stream
    /// resolvers are slow or transient; resolving them only for the selected
    /// row keeps valid search results from disappearing.
    async fn resolve_track(&self, track: &Track) -> Result<Track, String> {
        if track.audio_url.trim().is_empty() {
            Err("该音源暂时未能解析该歌曲的播放地址".into())
        } else {
            Ok(track.clone())
        }
    }
    async fn lyrics(&self, _track: &Track) -> Result<Option<String>, String> {
        Ok(None)
    }
}

pub fn registry(client: reqwest::Client) -> Vec<Arc<dyn MusicSource>> {
    vec![
        Arc::new(QqSource::new(client.clone())),
        Arc::new(KuwoSource::new(client.clone())),
        Arc::new(NeteaseSource::new(client.clone())),
        Arc::new(MiguSource::new(client)),
    ]
}

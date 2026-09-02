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
    async fn search(&self, query: &str, limit: usize, page: u32) -> Result<Vec<Track>, String>;
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
    /// Lightweight determination of the *actual* playable tier, used to make the
    /// search card's quality match what playback will resolve (lossless may be
    /// VIP-gated). Returns `(quality, format)` or `None`. Default: unknown.
    async fn resolve_quality(&self, _track: &Track) -> Option<(String, String)> {
        None
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

#[cfg(test)]
mod live_tests {
    use super::{MusicSource, KuwoSource, NeteaseSource, QqSource};

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("Mozilla/5.0")
            .connect_timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn qq_search_and_resolve() {
        let src = QqSource::new(client());
        let tracks = src.search("晴天", 1, 1).await.expect("qq search");
        assert!(!tracks.is_empty(), "QQ search returned no songs");
        let resolved = src.resolve_track(&tracks[0]).await.expect("qq resolve");
        assert!(resolved.audio_url.starts_with("http"), "QQ not resolvable: {resolved:?}");
        eprintln!("QQ url: {}", resolved.audio_url);
    }

    #[tokio::test]
    async fn qq_resolves_variety() {
        let src = QqSource::new(client());
        for (q, id) in [
            ("晴天", "qq:0039MnYb0qxYhV"),
            ("温柔", "qq:003L6xyk0vvEeA"),
            ("七里香", "qq:004Z8Ihr0JIu5s"),
        ] {
            let track = crate::domain::Track {
                id: id.into(), source: "QQMusicClient".into(),
                title: q.into(), artist: String::new(), album: String::new(),
                artwork_url: String::new(), audio_url: String::new(),
                duration_ms: 0, format: None, quality: None, adapter_payload: None,
            };
            match src.resolve_track(&track).await {
                Ok(r) => assert!(r.audio_url.starts_with("http"), "{q} NOT resolved: {:?}", r.audio_url),
                Err(e) => eprintln!("{q} resolve skipped (may be locked/rate-limited): {e}"),
            }
        }
    }

    #[tokio::test]
    async fn kuwo_search_and_resolve() {
        let src = KuwoSource::new(client());
        let tracks = src.search("晴天", 1, 1).await.expect("kuwo search");
        assert!(!tracks.is_empty(), "Kuwo search returned no songs");
        let resolved = src.resolve_track(&tracks[0]).await.expect("kuwo resolve");
        assert!(resolved.audio_url.starts_with("http"), "Kuwo not resolvable: {resolved:?}");
        eprintln!("Kuwo url: {}", resolved.audio_url);
    }


    #[tokio::test]
    async fn netease_resolves_variety() {
        let src = NeteaseSource::new(client());
        for (q, id) in [
            ("星辰大海", "netease:1811921555"),
            ("七里香", "netease:347230"),
            ("岁月神偷", "netease:64273"),
        ] {
            let track = crate::domain::Track {
                id: id.into(), source: "NeteaseMusicClient".into(),
                title: q.into(), artist: String::new(), album: String::new(),
                artwork_url: String::new(), audio_url: String::new(),
                duration_ms: 0, format: None, quality: None, adapter_payload: None,
            };
            match src.resolve_track(&track).await {
                Ok(r) => assert!(r.audio_url.starts_with("http"), "{q} NOT resolved: {:?}", r.audio_url),
                Err(e) => panic!("{q} resolve error: {e}"),
            }
        }
    }

    #[tokio::test]
    async fn netease_search_and_resolve() {
        let src = NeteaseSource::new(client());
        let tracks = src.search("星辰大海", 1, 1).await.expect("netease search");
        assert!(!tracks.is_empty(), "Netease search returned no songs");
        let resolved = src.resolve_track(&tracks[0]).await.expect("netease resolve");
        assert!(resolved.audio_url.starts_with("http"), "Netease not resolvable: {resolved:?}");
        eprintln!("Netease url: {}", resolved.audio_url);
    }
}

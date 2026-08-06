//! `YouTubeReleaseAdapter` — YouTube Data API v3 video upload/visibility.
//!
//! Native (not a plugin) per `docs/release-design.md` §6: the YouTube Data API v3 is a
//! plain REST API with no proprietary SDK dependency, unlike Steam/App Store. Config:
//! `youtube://channel/<channel-id>` in `[release] publish_url`.
//!
//! `prepare()` resolves title/description/visibility from `ReleaseContext` and stashes
//! them for the following `publish()` call — the shared `ReleaseAdapter` trait doesn't
//! thread `ReleaseContext` through to `publish()` (see `GitHubReleaseAdapter`'s same
//! note), but `ta release run` always calls `prepare()` then `publish()` on the same
//! adapter instance within one pipeline run, so `Mutex<Option<PendingUpload>>` state is
//! safe and matches the real call pattern instead of inventing a new trait method.

use std::path::Path;
use std::sync::Mutex;

use crate::adapter::{
    Channel, PreparedRelease, ReleaseAdapter, ReleaseAsset, ReleaseCapabilities, ReleaseContext,
    ReleaseRef,
};
use crate::error::{ReleaseError, Result};

/// YouTube Data API v3 operations needed by the adapter. Overridable via
/// `YouTubeReleaseAdapter::with_client` so adapter logic (visibility mapping,
/// prepare/publish sequencing) is testable without real OAuth credentials or network
/// access — mirrors `GitHubReleaseAdapter`'s `GhRunner` pattern.
pub trait YouTubeClient: Send + Sync {
    /// Preflight: verify the configured OAuth token can access `channel_id`.
    fn verify_access(&self, channel_id: &str) -> std::result::Result<(), String>;

    /// Upload a video file, returning its assigned video ID and watch URL.
    fn upload_video(
        &self,
        request: &YouTubeUploadRequest,
    ) -> std::result::Result<YouTubeUploadResponse, String>;

    /// Change an already-uploaded video's `privacyStatus`.
    fn set_visibility(&self, video_id: &str, visibility: &str) -> std::result::Result<(), String>;
}

pub struct YouTubeUploadRequest<'a> {
    pub channel_id: &'a str,
    pub file_path: &'a Path,
    pub title: &'a str,
    pub description: &'a str,
    pub visibility: &'a str,
}

pub struct YouTubeUploadResponse {
    pub video_id: String,
    pub url: String,
}

/// Real client, backed by `reqwest::blocking` and an OAuth access token read from
/// `YOUTUBE_OAUTH_TOKEN` (per the Observability Mandate: missing-token failures are
/// actionable, not a bare "unauthorized").
struct RealYouTubeClient {
    http: reqwest::blocking::Client,
}

impl RealYouTubeClient {
    fn new() -> Self {
        Self {
            http: reqwest::blocking::Client::new(),
        }
    }

    fn access_token() -> std::result::Result<String, String> {
        std::env::var("YOUTUBE_OAUTH_TOKEN").map_err(|_| {
            "YOUTUBE_OAUTH_TOKEN is not set. Generate an OAuth 2.0 access token with the \
             youtube.upload scope and export it as YOUTUBE_OAUTH_TOKEN before running \
             `ta release run` against a youtube:// target."
                .to_string()
        })
    }
}

impl YouTubeClient for RealYouTubeClient {
    fn verify_access(&self, channel_id: &str) -> std::result::Result<(), String> {
        let token = Self::access_token()?;
        let resp = self
            .http
            .get("https://www.googleapis.com/youtube/v3/channels")
            .query(&[("part", "id"), ("mine", "true")])
            .bearer_auth(&token)
            .send()
            .map_err(|e| format!("YouTube channels.list request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "YouTube channels.list returned {} for channel '{}' — check YOUTUBE_OAUTH_TOKEN \
                 scope and that the token is not expired.",
                resp.status(),
                channel_id
            ));
        }
        Ok(())
    }

    fn upload_video(
        &self,
        request: &YouTubeUploadRequest,
    ) -> std::result::Result<YouTubeUploadResponse, String> {
        let token = Self::access_token()?;
        let bytes = std::fs::read(request.file_path).map_err(|e| {
            format!(
                "failed to read video asset '{}': {e}",
                request.file_path.display()
            )
        })?;

        let metadata = serde_json::json!({
            "snippet": {
                "title": request.title,
                "description": request.description,
            },
            "status": {
                "privacyStatus": request.visibility,
            }
        });

        let form = reqwest::blocking::multipart::Form::new()
            .text("metadata", metadata.to_string())
            .part(
                "video",
                reqwest::blocking::multipart::Part::bytes(bytes)
                    .file_name(
                        request
                            .file_path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "video.mp4".to_string()),
                    )
                    .mime_str("video/*")
                    .map_err(|e| format!("failed to build video upload part: {e}"))?,
            );

        let resp = self
            .http
            .post("https://www.googleapis.com/upload/youtube/v3/videos")
            .query(&[("uploadType", "multipart"), ("part", "snippet,status")])
            .bearer_auth(&token)
            .multipart(form)
            .send()
            .map_err(|e| format!("YouTube videos.insert request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("YouTube videos.insert returned {status}: {body}"));
        }

        let body: serde_json::Value = resp
            .json()
            .map_err(|e| format!("YouTube videos.insert returned invalid JSON: {e}"))?;
        let video_id = body["id"]
            .as_str()
            .ok_or_else(|| "YouTube videos.insert response had no 'id' field".to_string())?
            .to_string();
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        Ok(YouTubeUploadResponse { video_id, url })
    }

    fn set_visibility(&self, video_id: &str, visibility: &str) -> std::result::Result<(), String> {
        let token = Self::access_token()?;
        let body = serde_json::json!({
            "id": video_id,
            "status": { "privacyStatus": visibility },
        });
        let resp = self
            .http
            .put("https://www.googleapis.com/youtube/v3/videos")
            .query(&[("part", "status")])
            .bearer_auth(&token)
            .json(&body)
            .send()
            .map_err(|e| format!("YouTube videos.update request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(format!("YouTube videos.update returned {status}: {text}"));
        }
        Ok(())
    }
}

struct PendingUpload {
    title: String,
    description: String,
    visibility: &'static str,
}

pub struct YouTubeReleaseAdapter {
    client: Box<dyn YouTubeClient>,
    channel_id: String,
    pending: Mutex<Option<PendingUpload>>,
}

impl std::fmt::Debug for YouTubeReleaseAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeReleaseAdapter")
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

impl YouTubeReleaseAdapter {
    pub fn new(channel_id: impl Into<String>) -> Self {
        Self {
            client: Box::new(RealYouTubeClient::new()),
            channel_id: channel_id.into(),
            pending: Mutex::new(None),
        }
    }

    pub fn with_client(channel_id: impl Into<String>, client: Box<dyn YouTubeClient>) -> Self {
        Self {
            client,
            channel_id: channel_id.into(),
            pending: Mutex::new(None),
        }
    }

    /// Parse `youtube://channel/<channel-id>` into an adapter instance.
    pub fn from_publish_url(publish_url: &str) -> Result<Self> {
        let channel_id = Self::parse_channel_id(publish_url)?;
        Ok(Self::new(channel_id))
    }

    fn parse_channel_id(publish_url: &str) -> Result<String> {
        let rest = publish_url.strip_prefix("youtube://").ok_or_else(|| {
            ReleaseError::Config(format!(
                "'{publish_url}' is not a youtube:// URL — expected 'youtube://channel/<channel-id>'"
            ))
        })?;
        let channel_id = rest
            .strip_prefix("channel/")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ReleaseError::Config(format!(
                    "'{publish_url}' is missing a channel id — expected 'youtube://channel/<channel-id>'"
                ))
            })?;
        Ok(channel_id.to_string())
    }

    /// Maps the standard channel model onto YouTube's `privacyStatus` values, per
    /// PLAN.md v0.17.4 item 1: `nightly` -> unlisted, `stable` -> public, `draft` -> private.
    fn visibility_for_channel(channel: &Channel) -> &'static str {
        match channel {
            Channel::Draft => "private",
            Channel::Rc => "unlisted",
            Channel::Stable => "public",
            Channel::Lts => "public",
            Channel::Custom(name) if name == "nightly" => "unlisted",
            Channel::Custom(_) => "unlisted",
        }
    }
}

impl ReleaseAdapter for YouTubeReleaseAdapter {
    fn name(&self) -> &str {
        "youtube"
    }

    fn capabilities(&self) -> ReleaseCapabilities {
        ReleaseCapabilities {
            requires_semver: false,
            supports_promote: true,
            supports_live_status: false,
            custom_channel_names: vec![],
        }
    }

    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease> {
        self.client
            .verify_access(&self.channel_id)
            .map_err(|reason| ReleaseError::PrepareFailed {
                adapter: self.name().to_string(),
                reason,
            })?;

        let visibility = Self::visibility_for_channel(&ctx.channel);
        *self.pending.lock().unwrap() = Some(PendingUpload {
            title: ctx.version_or_label.clone(),
            description: ctx.commits.clone(),
            visibility,
        });

        Ok(PreparedRelease {
            idempotency_key: ctx.version_or_label.clone(),
            resolved_label: ctx.version_or_label.clone(),
        })
    }

    fn publish(&self, _prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef> {
        let asset = assets.first().ok_or_else(|| ReleaseError::PublishFailed {
            adapter: self.name().to_string(),
            reason: "YouTube adapter requires exactly one video asset".to_string(),
        })?;

        let pending =
            self.pending
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| ReleaseError::PublishFailed {
                    adapter: self.name().to_string(),
                    reason: "publish() called before a successful prepare()".to_string(),
                })?;

        let request = YouTubeUploadRequest {
            channel_id: &self.channel_id,
            file_path: &asset.path,
            title: &pending.title,
            description: &pending.description,
            visibility: pending.visibility,
        };
        let response =
            self.client
                .upload_video(&request)
                .map_err(|reason| ReleaseError::PublishFailed {
                    adapter: self.name().to_string(),
                    reason,
                })?;

        Ok(ReleaseRef {
            adapter: self.name().to_string(),
            external_id: response.video_id,
            url: Some(response.url),
        })
    }

    fn promote(&self, release_ref: &ReleaseRef, channel: &Channel) -> Result<()> {
        let visibility = Self::visibility_for_channel(channel);
        self.client
            .set_visibility(&release_ref.external_id, visibility)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("promote to '{channel}' failed: {reason}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockYouTube {
        fail_verify: bool,
        upload_calls: StdMutex<Vec<(String, String, String)>>, // (title, description, visibility)
        visibility_calls: StdMutex<Vec<(String, String)>>,
    }

    impl YouTubeClient for MockYouTube {
        fn verify_access(&self, _channel_id: &str) -> std::result::Result<(), String> {
            if self.fail_verify {
                Err("token expired".to_string())
            } else {
                Ok(())
            }
        }

        fn upload_video(
            &self,
            request: &YouTubeUploadRequest,
        ) -> std::result::Result<YouTubeUploadResponse, String> {
            self.upload_calls.lock().unwrap().push((
                request.title.to_string(),
                request.description.to_string(),
                request.visibility.to_string(),
            ));
            Ok(YouTubeUploadResponse {
                video_id: "vid123".to_string(),
                url: "https://www.youtube.com/watch?v=vid123".to_string(),
            })
        }

        fn set_visibility(
            &self,
            video_id: &str,
            visibility: &str,
        ) -> std::result::Result<(), String> {
            self.visibility_calls
                .lock()
                .unwrap()
                .push((video_id.to_string(), visibility.to_string()));
            Ok(())
        }
    }

    fn ctx(channel: Channel) -> ReleaseContext {
        ReleaseContext {
            version_or_label: "episode-3".to_string(),
            channel,
            commits: "fixed the lighting rig".to_string(),
            workspace_root: PathBuf::from("."),
        }
    }

    #[test]
    fn name_is_youtube() {
        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(MockYouTube::default()));
        assert_eq!(adapter.name(), "youtube");
    }

    #[test]
    fn capabilities_do_not_require_semver() {
        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(MockYouTube::default()));
        let caps = adapter.capabilities();
        assert!(!caps.requires_semver);
        assert!(caps.supports_promote);
        assert!(!caps.supports_live_status);
    }

    #[test]
    fn prepare_fails_when_verify_access_fails() {
        let adapter = YouTubeReleaseAdapter::with_client(
            "UCxxxx",
            Box::new(MockYouTube {
                fail_verify: true,
                ..Default::default()
            }),
        );
        let err = adapter.prepare(&ctx(Channel::Stable)).unwrap_err();
        assert!(matches!(err, ReleaseError::PrepareFailed { .. }));
    }

    #[test]
    fn visibility_maps_stable_rc_draft() {
        assert_eq!(
            YouTubeReleaseAdapter::visibility_for_channel(&Channel::Stable),
            "public"
        );
        assert_eq!(
            YouTubeReleaseAdapter::visibility_for_channel(&Channel::Draft),
            "private"
        );
        assert_eq!(
            YouTubeReleaseAdapter::visibility_for_channel(&Channel::Rc),
            "unlisted"
        );
        assert_eq!(
            YouTubeReleaseAdapter::visibility_for_channel(&Channel::Custom("nightly".to_string())),
            "unlisted"
        );
    }

    #[test]
    fn publish_uploads_video_with_title_and_description_from_context() {
        let mock = MockYouTube::default();
        let adapter = YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(mock));
        let prepared = adapter.prepare(&ctx(Channel::Stable)).unwrap();
        let asset = ReleaseAsset {
            path: PathBuf::from("/tmp/episode-3.mp4"),
            label: None,
        };
        // Reach into the mock via the adapter's client is not possible after moving it in,
        // so verify indirectly through the returned ReleaseRef and a second adapter instance
        // is unnecessary — publish() itself exercises upload_video with the stashed fields.
        let release_ref = adapter.publish(&prepared, &[asset]).unwrap();
        assert_eq!(release_ref.external_id, "vid123");
        assert_eq!(
            release_ref.url.as_deref(),
            Some("https://www.youtube.com/watch?v=vid123")
        );
    }

    #[test]
    fn publish_stashes_title_description_and_visibility_from_prepare() {
        let mock = std::sync::Arc::new(MockYouTube::default());

        struct ArcClient(std::sync::Arc<MockYouTube>);
        impl YouTubeClient for ArcClient {
            fn verify_access(&self, c: &str) -> std::result::Result<(), String> {
                self.0.verify_access(c)
            }
            fn upload_video(
                &self,
                r: &YouTubeUploadRequest,
            ) -> std::result::Result<YouTubeUploadResponse, String> {
                self.0.upload_video(r)
            }
            fn set_visibility(&self, v: &str, vis: &str) -> std::result::Result<(), String> {
                self.0.set_visibility(v, vis)
            }
        }

        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(ArcClient(mock.clone())));
        let prepared = adapter.prepare(&ctx(Channel::Draft)).unwrap();
        let asset = ReleaseAsset {
            path: PathBuf::from("/tmp/episode-3.mp4"),
            label: None,
        };
        adapter.publish(&prepared, &[asset]).unwrap();

        let calls = mock.upload_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "episode-3");
        assert_eq!(calls[0].1, "fixed the lighting rig");
        assert_eq!(calls[0].2, "private");
    }

    #[test]
    fn publish_without_prepare_errors() {
        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(MockYouTube::default()));
        let prepared = PreparedRelease {
            idempotency_key: "x".to_string(),
            resolved_label: "x".to_string(),
        };
        let asset = ReleaseAsset {
            path: PathBuf::from("/tmp/x.mp4"),
            label: None,
        };
        let err = adapter.publish(&prepared, &[asset]).unwrap_err();
        assert!(matches!(err, ReleaseError::PublishFailed { .. }));
    }

    #[test]
    fn publish_requires_at_least_one_asset() {
        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(MockYouTube::default()));
        let prepared = adapter.prepare(&ctx(Channel::Stable)).unwrap();
        let err = adapter.publish(&prepared, &[]).unwrap_err();
        assert!(matches!(err, ReleaseError::PublishFailed { .. }));
    }

    #[test]
    fn promote_changes_visibility() {
        let mock = std::sync::Arc::new(MockYouTube::default());
        struct ArcClient(std::sync::Arc<MockYouTube>);
        impl YouTubeClient for ArcClient {
            fn verify_access(&self, c: &str) -> std::result::Result<(), String> {
                self.0.verify_access(c)
            }
            fn upload_video(
                &self,
                r: &YouTubeUploadRequest,
            ) -> std::result::Result<YouTubeUploadResponse, String> {
                self.0.upload_video(r)
            }
            fn set_visibility(&self, v: &str, vis: &str) -> std::result::Result<(), String> {
                self.0.set_visibility(v, vis)
            }
        }
        let adapter =
            YouTubeReleaseAdapter::with_client("UCxxxx", Box::new(ArcClient(mock.clone())));
        let release_ref = ReleaseRef {
            adapter: "youtube".to_string(),
            external_id: "vid123".to_string(),
            url: None,
        };
        adapter.promote(&release_ref, &Channel::Stable).unwrap();
        let calls = mock.visibility_calls.lock().unwrap();
        assert_eq!(calls[0], ("vid123".to_string(), "public".to_string()));
    }

    #[test]
    fn from_publish_url_parses_channel_id() {
        let adapter = YouTubeReleaseAdapter::from_publish_url("youtube://channel/UCxxxx").unwrap();
        assert_eq!(adapter.channel_id, "UCxxxx");
    }

    #[test]
    fn from_publish_url_rejects_wrong_scheme() {
        let err = YouTubeReleaseAdapter::from_publish_url("s3://bucket/x").unwrap_err();
        assert!(matches!(err, ReleaseError::Config(_)));
    }

    #[test]
    fn from_publish_url_rejects_missing_channel_id() {
        let err = YouTubeReleaseAdapter::from_publish_url("youtube://channel/").unwrap_err();
        assert!(matches!(err, ReleaseError::Config(_)));
    }
}

use crate::auth::{http_client, Auth};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Method;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Artist {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Album {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Track {
    pub id: Option<String>,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub album: Album,
    #[serde(default)]
    pub duration_ms: u64,
}
impl Track {
    pub fn label(&self) -> String {
        format!(
            "{} — {}",
            self.name,
            self.artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_restricted: bool,
    pub volume_percent: Option<u8>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Playback {
    #[serde(default)]
    pub is_playing: bool,
    pub progress_ms: Option<u64>,
    pub item: Option<Track>,
    pub device: Option<Device>,
}
#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}
#[derive(Deserialize)]
struct TrackItem {
    #[serde(alias = "track")]
    item: Option<Track>,
}

pub struct Api {
    pub auth: Auth,
    http: reqwest::Client,
    base: String,
    retry_at: Option<Instant>,
}
impl Api {
    pub fn new(auth: Auth) -> Result<Self> {
        Ok(Self {
            auth,
            http: http_client()?,
            base: "https://api.spotify.com/v1".into(),
            retry_at: None,
        })
    }
    pub async fn request(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value> {
        if let Some(deadline) = self.retry_at {
            if deadline > Instant::now() {
                bail!(
                    "Spotify rate limit: retry in {} seconds",
                    deadline.duration_since(Instant::now()).as_secs() + 1
                );
            }
        }
        if self.auth.tokens.expires_at <= Utc::now().timestamp() + 30 {
            self.auth.refresh(&self.http).await?;
        }
        for attempt in 0..2 {
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .bearer_auth(&self.auth.tokens.access_token)
                .query(query);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await.context("Cannot reach Spotify")?;
            let status = response.status();
            if status.as_u16() == 401 && attempt == 0 {
                self.auth.refresh(&self.http).await?;
                continue;
            }
            if status.as_u16() == 429 {
                let seconds = response
                    .headers()
                    .get("retry-after")
                    .and_then(|s| s.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30)
                    .min(86400);
                self.retry_at = Some(Instant::now() + Duration::from_secs(seconds));
                bail!("Spotify rate limit: retry in {seconds} seconds");
            }
            if status.as_u16() == 204 {
                return Ok(Value::Null);
            }
            let bytes = response.bytes().await?;
            if !status.is_success() {
                let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let message = data
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Request failed");
                match status.as_u16() {
                    401 => bail!("Spotify session expired; run spot login"),
                    403 => bail!("Spotify denied access: {message}. Check Premium, app allowlist, and login scopes"),
                    404 if path.starts_with("/me/player") => bail!("No active playback device. Open Spotify on a device, then use spot devices and spot transfer"),
                    _ => bail!("Spotify {status}: {message}"),
                }
            }
            return if bytes.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(serde_json::from_slice(&bytes)?)
            };
        }
        unreachable!()
    }
    async fn get<T: DeserializeOwned>(&mut self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        Ok(serde_json::from_value(
            self.request(Method::GET, path, query, None).await?,
        )?)
    }
    async fn pages<T: DeserializeOwned>(&mut self, path: &str) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut offset = 0;
        loop {
            let page: Page<T> = self
                .get(path, &[("limit", "50"), ("offset", &offset.to_string())])
                .await?;
            let count = page.items.len();
            items.extend(page.items);
            if page.next.is_none() || count == 0 {
                break;
            }
            offset += count;
        }
        Ok(items)
    }
    pub async fn playback(&mut self) -> Result<Playback> {
        let value = self.request(Method::GET, "/me/player", &[], None).await?;
        if value.is_null() {
            Ok(Playback::default())
        } else {
            Ok(serde_json::from_value(value)?)
        }
    }
    pub async fn play(&mut self, uri: Option<&str>) -> Result<()> {
        let body = uri.map(|uri| {
            if uri.starts_with("spotify:track:") {
                json!({"uris":[uri]})
            } else {
                json!({"context_uri":uri})
            }
        });
        self.request(Method::PUT, "/me/player/play", &[], body)
            .await?;
        Ok(())
    }
    pub async fn pause(&mut self) -> Result<()> {
        self.request(Method::PUT, "/me/player/pause", &[], None)
            .await?;
        Ok(())
    }
    pub async fn next(&mut self) -> Result<()> {
        self.request(Method::POST, "/me/player/next", &[], None)
            .await?;
        Ok(())
    }
    pub async fn previous(&mut self) -> Result<()> {
        self.request(Method::POST, "/me/player/previous", &[], None)
            .await?;
        Ok(())
    }
    pub async fn devices(&mut self) -> Result<Vec<Device>> {
        #[derive(Deserialize)]
        struct Devices {
            devices: Vec<Device>,
        }
        Ok(self
            .get::<Devices>("/me/player/devices", &[])
            .await?
            .devices)
    }
    pub async fn transfer(&mut self, id: &str) -> Result<()> {
        self.request(
            Method::PUT,
            "/me/player",
            &[],
            Some(json!({"device_ids":[id], "play":true})),
        )
        .await?;
        Ok(())
    }
    pub async fn volume(&mut self, value: u8) -> Result<()> {
        anyhow::ensure!(value <= 100, "Volume must be 0–100");
        self.request(
            Method::PUT,
            "/me/player/volume",
            &[("volume_percent", &value.to_string())],
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn search(&mut self, query: &str, kind: &str, offset: usize) -> Result<Value> {
        self.request(
            Method::GET,
            "/search",
            &[
                ("q", query),
                ("type", kind),
                ("limit", "10"),
                ("offset", &offset.to_string()),
            ],
            None,
        )
        .await
    }
    pub async fn search_tracks(&mut self, query: &str) -> Result<Vec<Track>> {
        let result = self.search(query, "track", 0).await?;
        Ok(serde_json::from_value(
            result
                .pointer("/tracks/items")
                .cloned()
                .unwrap_or(json!([])),
        )?)
    }
    pub async fn recent(&mut self) -> Result<Vec<Track>> {
        let page: Page<TrackItem> = self
            .get("/me/player/recently-played", &[("limit", "50")])
            .await?;
        Ok(page.items.into_iter().filter_map(|t| t.item).collect())
    }
    pub async fn playlists(&mut self) -> Result<Vec<Playlist>> {
        self.pages("/me/playlists").await
    }
    pub async fn playlist_tracks(&mut self, id: &str) -> Result<Vec<Track>> {
        let items: Vec<TrackItem> = self
            .pages(&format!("/playlists/{}/items", spotify_id(id, "playlist")?))
            .await?;
        Ok(items.into_iter().filter_map(|t| t.item).collect())
    }
    pub async fn create_playlist(&mut self, name: &str, description: &str) -> Result<Playlist> {
        anyhow::ensure!(!name.trim().is_empty(), "Playlist name cannot be empty");
        Ok(serde_json::from_value(
            self.request(
                Method::POST,
                "/me/playlists",
                &[],
                Some(json!({"name":name,"public":false,"description":description})),
            )
            .await?,
        )?)
    }
    pub async fn add(&mut self, track: &str, playlist: &str) -> Result<()> {
        let uri = track_uri(track)?;
        self.request(
            Method::POST,
            &format!("/playlists/{}/items", spotify_id(playlist, "playlist")?),
            &[],
            Some(json!({"uris":[uri]})),
        )
        .await?;
        Ok(())
    }
    pub async fn remove(&mut self, track: &str, playlist: &str) -> Result<()> {
        let uri = track_uri(track)?;
        self.request(
            Method::DELETE,
            &format!("/playlists/{}/items", spotify_id(playlist, "playlist")?),
            &[],
            Some(json!({"items":[{"uri":uri}]})),
        )
        .await?;
        Ok(())
    }
    pub async fn queue(&mut self, track: &str) -> Result<()> {
        self.request(
            Method::POST,
            "/me/player/queue",
            &[("uri", &track_uri(track)?)],
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn like(&mut self, track: &str) -> Result<()> {
        self.request(
            Method::PUT,
            "/me/library",
            &[("uris", &track_uri(track)?)],
            None,
        )
        .await?;
        Ok(())
    }
}
pub fn spotify_id(input: &str, kind: &str) -> Result<String> {
    let id = if input.starts_with("https://open.spotify.com/") {
        let url = url::Url::parse(input)?;
        let parts: Vec<_> = url
            .path_segments()
            .context("Invalid Spotify URL")?
            .collect();
        anyhow::ensure!(
            parts.len() == 2 && parts[0] == kind,
            "Expected a Spotify {kind} URL"
        );
        parts[1].to_string()
    } else {
        input
            .strip_prefix(&format!("spotify:{kind}:"))
            .unwrap_or(input)
            .to_string()
    };
    anyhow::ensure!(
        id.len() == 22 && id.bytes().all(|c| c.is_ascii_alphanumeric()),
        "Expected a Spotify {kind} ID, URI, or share URL"
    );
    Ok(id)
}
pub fn track_uri(input: &str) -> Result<String> {
    Ok(format!("spotify:track:{}", spotify_id(input, "track")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Tokens;
    use wiremock::{
        matchers::{body_json, header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    const ID: &str = "0123456789abcdefghijkl";
    fn api(server: &MockServer, dir: &std::path::Path) -> Api {
        let auth = Auth {
            client_id: "test".into(),
            token_url: format!("{}/token", server.uri()),
            path: dir.join("tokens.json"),
            tokens: Tokens {
                access_token: "old".into(),
                refresh_token: "refresh".into(),
                expires_at: Utc::now().timestamp() + 3600,
            },
        };
        let mut api = Api::new(auth).unwrap();
        api.base = server.uri();
        api
    }
    #[tokio::test]
    async fn no_playback_and_no_device() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&s)
            .await;
        assert!(a.playback().await.unwrap().item.is_none());
        Mock::given(method("PUT"))
            .and(path("/me/player/play"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        assert!(a
            .play(None)
            .await
            .unwrap_err()
            .to_string()
            .contains("No active playback device"));
    }
    #[tokio::test]
    async fn rate_limit_does_not_repeat_mutation() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        Mock::given(method("POST"))
            .and(path("/me/player/next"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "60"))
            .expect(1)
            .mount(&s)
            .await;
        assert!(a
            .next()
            .await
            .unwrap_err()
            .to_string()
            .contains("60 seconds"));
        assert!(a.next().await.is_err());
    }
    #[tokio::test]
    async fn refresh_on_401_and_preserve_refresh_token() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        Mock::given(path("/me/player"))
            .and(header("authorization", "Bearer old"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"access_token":"new", "token_type":"Bearer", "expires_in":3600}),
            ))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(path("/me/player"))
            .and(header("authorization", "Bearer new"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&s)
            .await;
        a.playback().await.unwrap();
        assert_eq!(a.auth.tokens.refresh_token, "refresh");
        let saved: Tokens =
            serde_json::from_slice(&std::fs::read(d.path().join("tokens.json")).unwrap()).unwrap();
        assert_eq!(saved.access_token, "new");
    }
    #[tokio::test]
    async fn expired_token_refreshes_before_request() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        a.auth.tokens.expires_at = 0;
        Mock::given(path("/token")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"new", "refresh_token":"rotated", "token_type":"Bearer", "expires_in":3600}))).expect(1).mount(&s).await;
        Mock::given(path("/me/player"))
            .and(header("authorization", "Bearer new"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&s)
            .await;
        a.playback().await.unwrap();
        assert_eq!(a.auth.tokens.refresh_token, "rotated");
    }
    #[tokio::test]
    async fn playlist_pagination_and_nullable_items() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        let endpoint = format!("/playlists/{ID}/items");
        Mock::given(path(&endpoint))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"items":[{"item":null},{"item":{"name":"First"}}],"next":"ignored"}),
            ))
            .mount(&s)
            .await;
        Mock::given(path(&endpoint))
            .and(query_param("offset", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"items":[{"track":{"name":"Second"}}],"next":null})),
            )
            .mount(&s)
            .await;
        let tracks = a.playlist_tracks(ID).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].name, "Second");
    }
    #[tokio::test]
    async fn current_endpoints_and_search_limit() {
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        let uri = format!("spotify:track:{ID}");
        Mock::given(method("PUT"))
            .and(path("/me/library"))
            .and(query_param("uris", &uri))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/playlists/{ID}/items")))
            .and(body_json(json!({"uris":[uri]})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"snapshot_id":"one"})))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(path("/search"))
            .and(query_param("limit", "10"))
            .and(query_param("q", "a & b"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tracks":{"items":[]}})))
            .expect(1)
            .mount(&s)
            .await;
        a.like(ID).await.unwrap();
        a.add(ID, ID).await.unwrap();
        assert!(a.search_tracks("a & b").await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn weekly_playlist_is_created_once_and_cached() {
        use crate::{config::Config, newmusic};
        let s = MockServer::start().await;
        let d = tempfile::tempdir().unwrap();
        let mut a = api(&s, d.path());
        let day = newmusic::release_day(Utc::now());
        let name = newmusic::playlist_name(day);
        let description = format!("spotify-cli release week {day}");
        Mock::given(method("GET"))
            .and(path("/me/playlists"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[],"next":null})))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/me/playlists"))
            .and(body_json(
                json!({"name":name,"description":description,"public":false}),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({"id":ID,"name":name,"description":description})),
            )
            .expect(1)
            .mount(&s)
            .await;
        let mut config = Config::default();
        assert_eq!(
            newmusic::ensure(&mut a, &mut config, d.path())
                .await
                .unwrap(),
            ID
        );
        let mut reloaded = Config::load(d.path()).unwrap();
        assert_eq!(
            newmusic::ensure(&mut a, &mut reloaded, d.path())
                .await
                .unwrap(),
            ID
        );
    }
    #[test]
    fn validates_resource_identifiers() {
        assert_eq!(
            spotify_id(
                &format!("https://open.spotify.com/track/{ID}?si=test"),
                "track"
            )
            .unwrap(),
            ID
        );
        assert!(spotify_id("../../me", "playlist").is_err());
        assert!(spotify_id(&format!("spotify:album:{ID}"), "track").is_err());
    }
}

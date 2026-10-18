use crate::config::write_private;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub const REDIRECT: &str = "http://127.0.0.1:8888/callback";
const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-library-modify",
    // Required by the Web Playback SDK (see webplayer.rs).
    "streaming",
    "user-read-email",
    "user-read-private",
];
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}
pub struct Auth {
    pub client_id: String,
    pub tokens: Tokens,
    pub token_url: String,
    pub path: PathBuf,
}
type OAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;
fn client(id: &str, token_url: &str) -> Result<OAuthClient> {
    Ok(BasicClient::new(ClientId::new(id.into()))
        .set_auth_uri(AuthUrl::new(
            "https://accounts.spotify.com/authorize".into(),
        )?)
        .set_token_uri(TokenUrl::new(token_url.into())?)
        .set_redirect_uri(RedirectUrl::new(REDIRECT.into())?))
}
pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?)
}
impl Auth {
    pub fn load(id: &str, dir: &Path) -> Result<Self> {
        anyhow::ensure!(
            !id.is_empty(),
            "Run spot login --client-id YOUR_CLIENT_ID first"
        );
        let path = dir.join("tokens.json");
        let tokens = serde_json::from_slice(&std::fs::read(&path).context("Run spot login first")?)
            .context("Invalid token file; run spot login again")?;
        Ok(Self {
            client_id: id.into(),
            tokens,
            token_url: "https://accounts.spotify.com/api/token".into(),
            path,
        })
    }
    pub fn save(&self) -> Result<()> {
        write_private(&self.path, &serde_json::to_vec(&self.tokens)?)
    }
    pub async fn refresh(&mut self, http: &reqwest::Client) -> Result<()> {
        let token = client(&self.client_id, &self.token_url)?
            .exchange_refresh_token(&RefreshToken::new(self.tokens.refresh_token.clone()))
            .request_async(http)
            .await
            .map_err(|_| {
                anyhow::anyhow!("Token refresh failed; check connectivity or run spot login again")
            })?;
        self.tokens.access_token = token.access_token().secret().clone();
        if let Some(refresh) = token.refresh_token() {
            self.tokens.refresh_token = refresh.secret().clone();
        }
        self.tokens.expires_at = Utc::now().timestamp()
            + token
                .expires_in()
                .unwrap_or(Duration::from_secs(3600))
                .as_secs() as i64;
        self.save()
    }
}
fn callback_code(target: &str, expected_state: &str) -> Result<String> {
    let url = url::Url::parse(&format!("http://127.0.0.1:8888{target}"))?;
    anyhow::ensure!(url.path() == "/callback", "Unexpected callback path");
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
    anyhow::ensure!(
        query.get("state").map(|s| s.as_ref()) == Some(expected_state),
        "OAuth state mismatch"
    );
    if query.contains_key("error") {
        bail!("Spotify authorization was declined");
    }
    Ok(query
        .get("code")
        .context("Missing authorization code")?
        .to_string())
}
pub async fn login(id: &str, dir: &Path) -> Result<()> {
    anyhow::ensure!(
        !id.trim().is_empty(),
        "Supply --client-id from your Spotify developer app"
    );
    let listener = TcpListener::bind("127.0.0.1:8888")
        .await
        .context("Cannot listen on port 8888; close the process using it")?;
    let oauth = client(id, "https://accounts.spotify.com/api/token")?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = oauth
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    for scope in SCOPES {
        request = request.add_scope(Scope::new((*scope).into()));
    }
    let (url, state) = request.url();
    println!(
        "Open this URL in your browser to sign in:\n\n{url}\n\nWaiting for Spotify (5 minutes)..."
    );
    let code = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(5), async {
                let mut chunk = [0; 1024];
                while buffer.len() < 8192 {
                    let count = stream.read(&mut chunk).await?;
                    if count == 0 { break; }
                    buffer.extend_from_slice(&chunk[..count]);
                    if buffer.windows(4).any(|w| w == b"\r\n\r\n") { return Ok::<_, std::io::Error>(true); }
                }
                Ok(false)
            }).await;
            if !matches!(read, Ok(Ok(true))) { continue; }
            let request = String::from_utf8_lossy(&buffer);
            let target = request.lines().next().and_then(|s| s.strip_prefix("GET ")).and_then(|s| s.split_whitespace().next()).unwrap_or("");
            let result = callback_code(target, state.secret());
            let (status, message) = if result.is_ok() { ("200 OK", "Authorization received. You can return to the terminal.") } else { ("400 Bad Request", "Invalid authorization callback. Return to the terminal or retry the login link.") };
            let reply = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}", message.len());
            let _ = stream.write_all(reply.as_bytes()).await;
            if let Ok(code) = result { return Ok::<_, anyhow::Error>(code); }
        }
    }).await.context("Login timed out; run spot login again")??;
    let token = oauth
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http_client()?)
        .await
        .map_err(|_| {
            anyhow::anyhow!("Authorization exchange failed; verify your redirect URI and try again")
        })?;
    Auth {
        client_id: id.into(),
        token_url: "https://accounts.spotify.com/api/token".into(),
        path: dir.join("tokens.json"),
        tokens: Tokens {
            access_token: token.access_token().secret().clone(),
            refresh_token: token
                .refresh_token()
                .context("Spotify did not return a refresh token")?
                .secret()
                .clone(),
            expires_at: Utc::now().timestamp()
                + token
                    .expires_in()
                    .unwrap_or(Duration::from_secs(3600))
                    .as_secs() as i64,
        },
    }
    .save()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_state_and_correct_path() {
        assert_eq!(
            callback_code("/callback?code=abc&state=secret", "secret").unwrap(),
            "abc"
        );
        assert!(callback_code("/callback?code=abc&state=wrong", "secret").is_err());
        assert!(callback_code("/callback?code=abc", "secret").is_err());
        assert!(callback_code("/wrong?code=abc&state=secret", "secret").is_err());
        assert!(callback_code("/callback?error=access_denied&state=secret", "secret").is_err());
    }
}

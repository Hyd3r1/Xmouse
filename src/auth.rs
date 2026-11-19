//! Microsoft account sign-in (device code flow) and the Xbox token chain needed for remote play.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const CLIENT_ID: &str = "1f907974-e22b-4810-a9de-d9647380c97e";
const SCOPE: &str = "xboxlive.signin openid profile offline_access";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserToken {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Clone, Debug)]
struct Xsts {
    token: String,
    uhs: String,
    expires_at: u64,
}

#[derive(Clone, Debug)]
pub struct StreamToken {
    pub host: String,
    pub token: String,
    pub expires_at: u64,
}

#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Default)]
struct Cache {
    user: Option<UserToken>,
    user_xsts: Option<Xsts>,
    gssv: Option<Xsts>,
    web: Option<Xsts>,
    stream: Option<StreamToken>,
}

pub struct Auth {
    file: PathBuf,
    http: reqwest::Client,
    cache: Mutex<Cache>,
}

fn parse_xsts(v: &Value) -> Result<Xsts> {
    let token = v["Token"]
        .as_str()
        .context("XSTS response without token")?
        .to_string();
    let uhs = v["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // Tokens last ~16 h; refresh well before.
    Ok(Xsts {
        token,
        uhs,
        expires_at: now() + 3600,
    })
}

impl Auth {
    pub fn new(file: PathBuf) -> Self {
        let user = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| serde_json::from_str::<UserToken>(&s).ok());
        Self {
            file,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client"),
            cache: Mutex::new(Cache {
                user,
                ..Default::default()
            }),
        }
    }

    pub async fn is_signed_in(&self) -> bool {
        self.cache.lock().await.user.is_some()
    }

    pub async fn sign_out(&self) {
        *self.cache.lock().await = Cache::default();
        let _ = std::fs::remove_file(&self.file);
    }

    fn save(&self, user: &UserToken) {
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(user) {
            let _ = std::fs::write(&self.file, s);
        }
    }

    async fn form(&self, url: &str, params: &[(&str, &str)]) -> Result<(u16, Value)> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();
        let res = self
            .http
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Cache-Control", "no-store, must-revalidate, no-cache")
            .body(body)
            .send()
            .await?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        Ok((status, v))
    }

    /// Step 1 of sign-in: get the code the user types at microsoft.com/link.
    pub async fn start_device_code(&self) -> Result<DeviceCode> {
        let (status, v) = self
            .form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
                &[("client_id", CLIENT_ID), ("scope", SCOPE)],
            )
            .await?;
        if status != 200 {
            bail!(
                "Microsoft sign-in returned HTTP {status}: {}",
                v["error_description"].as_str().unwrap_or("")
            );
        }
        Ok(DeviceCode {
            user_code: v["user_code"].as_str().unwrap_or_default().into(),
            device_code: v["device_code"].as_str().unwrap_or_default().into(),
            verification_uri: v["verification_uri"]
                .as_str()
                .unwrap_or("https://www.microsoft.com/link")
                .into(),
            expires_in: v["expires_in"].as_u64().unwrap_or(900),
            interval: v["interval"].as_u64().unwrap_or(5),
        })
    }

    /// Step 2: wait until the user has entered the code.
    pub async fn wait_for_device_code(&self, code: &DeviceCode) -> Result<()> {
        let deadline = now() + code.expires_in;
        let mut interval = code.interval.max(1);
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if now() > deadline {
                bail!("The sign-in code expired. Try again.");
            }
            let (status, v) = self
                .form(
                    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("client_id", CLIENT_ID),
                        ("device_code", &code.device_code),
                    ],
                )
                .await?;
            if status == 200 {
                let user = user_from(&v)?;
                self.save(&user);
                *self.cache.lock().await = Cache {
                    user: Some(user),
                    ..Default::default()
                };
                return Ok(());
            }
            match v["error"].as_str().unwrap_or("") {
                "authorization_pending" => {}
                "slow_down" => interval += 5,
                "authorization_declined" => bail!("Sign-in was declined."),
                "expired_token" => bail!("The sign-in code expired. Try again."),
                other => bail!(
                    "Sign-in failed: {other} {}",
                    v["error_description"].as_str().unwrap_or("")
                ),
            }
        }
    }

    async fn user_access_token(&self) -> Result<String> {
        let mut cache = self.cache.lock().await;
        let user = cache.user.clone().ok_or_else(|| anyhow!("Not signed in"))?;
        if user.expires_at > now() + 60 {
            return Ok(user.access_token);
        }
        drop(cache);
        let (status, v) = self
            .form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                &[
                    ("client_id", CLIENT_ID),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &user.refresh_token),
                    ("scope", SCOPE),
                ],
            )
            .await?;
        if status != 200 {
            bail!("Your sign-in expired. Please sign in again. (HTTP {status})");
        }
        let fresh = user_from(&v)?;
        self.save(&fresh);
        cache = self.cache.lock().await;
        cache.user = Some(fresh.clone());
        Ok(fresh.access_token)
    }

    async fn xsts_post(&self, url: &str, body: Value) -> Result<Value> {
        let res = self
            .http
            .post(url)
            .header("x-xbl-contract-version", "1")
            .header("Cache-Control", "no-cache")
            .header("Origin", "https://www.xbox.com")
            .header("Referer", "https://www.xbox.com/")
            .header("Accept", "*/*")
            .header("ms-cv", "0")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            if text.contains("2148916233") {
                bail!("This Microsoft account has no Xbox profile yet. Sign in on xbox.com once to create one.");
            }
            bail!("Xbox sign-in failed (HTTP {}) {}", status.as_u16(), text);
        }
        Ok(res.json().await?)
    }

    async fn user_xsts(&self) -> Result<Xsts> {
        if let Some(x) = self.cache.lock().await.user_xsts.clone() {
            if x.expires_at > now() {
                return Ok(x);
            }
        }
        let access = self.user_access_token().await?;
        let v = self
            .xsts_post(
                "https://user.auth.xboxlive.com/user/authenticate",
                json!({
                    "Properties": { "AuthMethod": "RPS", "RpsTicket": format!("d={access}"), "SiteName": "user.auth.xboxlive.com" },
                    "RelyingParty": "http://auth.xboxlive.com",
                    "TokenType": "JWT"
                }),
            )
            .await?;
        let x = parse_xsts(&v)?;
        self.cache.lock().await.user_xsts = Some(x.clone());
        Ok(x)
    }

    async fn authorize(&self, relying_party: &str) -> Result<Xsts> {
        let user = self.user_xsts().await?;
        let v = self
            .xsts_post(
                "https://xsts.auth.xboxlive.com/xsts/authorize",
                json!({
                    "Properties": { "SandboxId": "RETAIL", "UserTokens": [user.token] },
                    "RelyingParty": relying_party,
                    "TokenType": "JWT"
                }),
            )
            .await?;
        parse_xsts(&v)
    }

    /// Token for the Xbox home streaming ("xhome") service, and the region host to use.
    pub async fn stream_token(&self) -> Result<StreamToken> {
        if let Some(s) = self.cache.lock().await.stream.clone() {
            if s.expires_at > now() + 300 {
                return Ok(s);
            }
        }
        let gssv = {
            let cached = self.cache.lock().await.gssv.clone();
            match cached {
                Some(g) if g.expires_at > now() => g,
                _ => {
                    let g = self.authorize("http://gssv.xboxlive.com/").await?;
                    self.cache.lock().await.gssv = Some(g.clone());
                    g
                }
            }
        };
        let res = self
            .http
            .post("https://xhome.gssv-play-prod.xboxlive.com/v2/login/user")
            .header("Cache-Control", "no-store, must-revalidate, no-cache")
            .header("x-gssv-client", "XboxComBrowser")
            .json(&json!({ "token": gssv.token, "offeringId": "xhome" }))
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            bail!(
                "Xbox remote play is not available for this account (HTTP {}).",
                status.as_u16()
            );
        }
        let v: Value = res.json().await?;
        let regions = v["offeringSettings"]["regions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let region = regions
            .iter()
            .find(|r| r["isDefault"].as_bool() == Some(true))
            .or(regions.first())
            .context("No streaming region returned")?;
        let host = region["baseUri"]
            .as_str()
            .context("Region without baseUri")?
            .trim_end_matches('/')
            .to_string();
        let token = v["gsToken"].as_str().context("No gsToken")?.to_string();
        let duration = v["durationInSeconds"].as_u64().unwrap_or(3600);
        let s = StreamToken {
            host,
            token,
            expires_at: now() + duration,
        };
        self.cache.lock().await.stream = Some(s.clone());
        Ok(s)
    }

    /// Short-lived token the console uses to sign the session in as you.
    pub async fn console_transfer_token(&self) -> Result<String> {
        self.user_access_token().await?; // make sure the refresh token is current
        let refresh = self
            .cache
            .lock()
            .await
            .user
            .clone()
            .context("Not signed in")?
            .refresh_token;
        let (status, v) = self
            .form(
                "https://login.live.com/oauth20_token.srf",
                &[
                    ("client_id", CLIENT_ID),
                    ("scope", "service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN"),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh),
                ],
            )
            .await?;
        if status != 200 {
            bail!("Could not get a console sign-in token (HTTP {status})");
        }
        Ok(v["access_token"]
            .as_str()
            .context("No access_token")?
            .to_string())
    }

    /// Xbox Live web token (used to wake the console).
    pub async fn web_token(&self) -> Result<(String, String)> {
        if let Some(w) = self.cache.lock().await.web.clone() {
            if w.expires_at > now() {
                return Ok((w.token, w.uhs));
            }
        }
        let w = self.authorize("http://xboxlive.com").await?;
        self.cache.lock().await.web = Some(w.clone());
        Ok((w.token, w.uhs))
    }
}

fn user_from(v: &Value) -> Result<UserToken> {
    Ok(UserToken {
        access_token: v["access_token"]
            .as_str()
            .context("No access_token")?
            .into(),
        refresh_token: v["refresh_token"]
            .as_str()
            .context("No refresh_token")?
            .into(),
        expires_at: now() + v["expires_in"].as_u64().unwrap_or(3600),
    })
}

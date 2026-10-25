//! Microsoft 账户设备码登录。
//!
//! 流程：设备码 → OAuth 令牌 → Xbox Live → XSTS → Minecraft → 档案。
//! 需要设置 `ARKMCL_MSA_CLIENT_ID` 为已注册的 Azure 应用客户端 ID。

use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

const DEVICE_CODE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBOX: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MINECRAFT: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";

/// 登录配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// Azure 应用客户端 ID。
    pub client_id: String,
}

impl Config {
    /// 从 `ARKMCL_MSA_CLIENT_ID` 读取配置。
    ///
    /// # Errors
    ///
    /// 环境变量缺失或为空时返回错误。
    pub fn from_env() -> Result<Self, Error> {
        std::env::var("ARKMCL_MSA_CLIENT_ID")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(|client_id| Self { client_id })
            .ok_or(Error::MissingClientId)
    }
}

/// 设备码。
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    /// 轮询用设备码。
    pub device_code: String,
    /// 用户输入的短码。
    pub user_code: String,
    /// 验证地址。
    pub verification_uri: String,
    /// 有效期（秒）。
    pub expires_in: u64,
    /// 轮询间隔（秒）。
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// 面向用户的提示。
    #[serde(default)]
    pub message: String,
}

fn default_interval() -> u64 {
    5
}

/// Minecraft 档案。
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    /// 玩家 UUID。
    pub id: String,
    /// 玩家名。
    pub name: String,
}

/// 登录结果。
#[derive(Debug, Clone)]
pub struct Login {
    /// Minecraft 访问令牌。
    pub access_token: String,
    /// OAuth 刷新令牌。
    pub refresh_token: Option<String>,
    /// 访问令牌有效期（秒）。
    pub expires_in: Option<u64>,
    /// 玩家档案。
    pub profile: Profile,
}

/// 登录错误。
#[derive(Debug)]
pub enum Error {
    /// 未配置客户端 ID。
    MissingClientId,
    /// HTTP 错误。
    Http(String),
    /// JSON 错误。
    Json(serde_json::Error),
    /// OAuth 错误。
    Auth(String),
    /// 设备码过期。
    Expired,
    /// Xbox 响应缺少必要字段。
    Claims,
    /// 用户没有 Minecraft 档案。
    NoProfile,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingClientId => formatter.write_str("ARKMCL_MSA_CLIENT_ID is not set"),
            Self::Http(error) | Self::Auth(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Expired => formatter.write_str("device code expired"),
            Self::Claims => formatter.write_str("Xbox response is missing claims"),
            Self::NoProfile => formatter.write_str("account has no Minecraft profile"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::MissingClientId
            | Self::Http(_)
            | Self::Auth(_)
            | Self::Expired
            | Self::Claims
            | Self::NoProfile => None,
        }
    }
}

/// 开始设备码登录，并在轮询完成后返回 Minecraft 令牌与档案。
///
/// `notify` 会收到设备码，用于提示用户打开验证地址。
///
/// # Errors
///
/// 网络、OAuth、Xbox 或档案请求失败时返回错误。
pub fn login<F>(config: &Config, notify: F) -> Result<Login, Error>
where
    F: FnOnce(&DeviceCode),
{
    let code = begin(config)?;
    notify(&code);
    let token = poll(config, &code)?;
    let xbox = xbox(&token.access_token)?;
    let xsts = xsts(&xbox.token, &xbox.uhs)?;
    let access_token = minecraft(&xsts.token, &xsts.uhs)?;
    let profile = profile(&access_token)?;
    Ok(Login {
        access_token,
        refresh_token: token.refresh_token,
        expires_in: token.expires_in,
        profile,
    })
}

/// 使用刷新令牌获取新的 Minecraft 访问令牌。
///
/// # Errors
///
/// 网络、OAuth、Xbox 或档案请求失败时返回错误。
pub fn refresh(config: &Config, refresh_token: &str) -> Result<Login, Error> {
    let token = refresh_request(config, refresh_token)?;
    let xbox = xbox(&token.access_token)?;
    let xsts = xsts(&xbox.token, &xbox.uhs)?;
    let access_token = minecraft(&xsts.token, &xsts.uhs)?;
    let profile = profile(&access_token)?;
    Ok(Login {
        access_token,
        refresh_token: token
            .refresh_token
            .or_else(|| Some(refresh_token.to_owned())),
        expires_in: token.expires_in,
        profile,
    })
}

fn refresh_request(config: &Config, refresh_token: &str) -> Result<TokenResponse, Error> {
    let result = ureq::post(TOKEN)
        .timeout(Duration::from_secs(30))
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &config.client_id),
            ("refresh_token", refresh_token),
            ("scope", "XboxLive.signin offline_access"),
        ]);
    match result {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|error| Error::Http(error.to_string()))?;
            serde_json::from_str(&text).map_err(Error::Json)
        }
        Err(ureq::Error::Status(_, response)) => {
            let text = response
                .into_string()
                .map_err(|error| Error::Http(error.to_string()))?;
            let error: OAuthError =
                serde_json::from_str(&text).map_err(|error| Error::Auth(error.to_string()))?;
            Err(Error::Auth(format!(
                "{}: {}",
                error.error, error.error_description
            )))
        }
        Err(error) => Err(Error::Http(error.to_string())),
    }
}

fn begin(config: &Config) -> Result<DeviceCode, Error> {
    let text = post_form(
        DEVICE_CODE,
        &[
            ("client_id", &config.client_id),
            ("scope", "XboxLive.signin offline_access"),
        ],
    )?;
    serde_json::from_str(&text).map_err(Error::Json)
}

fn poll(config: &Config, code: &DeviceCode) -> Result<TokenResponse, Error> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in.max(1));
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Expired);
        }
        thread::sleep(Duration::from_secs(code.interval.max(1)));
        let result = ureq::post(TOKEN)
            .timeout(Duration::from_secs(30))
            .send_form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", &config.client_id),
                ("device_code", &code.device_code),
            ]);
        match result {
            Ok(response) => {
                let text = response
                    .into_string()
                    .map_err(|error| Error::Http(error.to_string()))?;
                return serde_json::from_str(&text).map_err(Error::Json);
            }
            Err(ureq::Error::Status(_, response)) => {
                let text = response
                    .into_string()
                    .map_err(|error| Error::Http(error.to_string()))?;
                let error: OAuthError =
                    serde_json::from_str(&text).map_err(|error| Error::Auth(error.to_string()))?;
                match error.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => thread::sleep(Duration::from_secs(5)),
                    other => {
                        return Err(Error::Auth(format!("{other}: {}", error.error_description)));
                    }
                }
            }
            Err(error) => return Err(Error::Http(error.to_string())),
        }
    }
}

fn xbox(access_token: &str) -> Result<Xbox, Error> {
    let body = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={access_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });
    let value = post_json(XBOX, &body)?;
    let token = value["Token"].as_str().ok_or(Error::Claims)?.to_owned();
    let uhs = value["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or(Error::Claims)?
        .to_owned();
    Ok(Xbox { token, uhs })
}

fn xsts(xbox: &str, uhs: &str) -> Result<Xbox, Error> {
    let body = json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbox]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });
    let value = post_json(XSTS, &body)?;
    let token = value["Token"].as_str().ok_or(Error::Claims)?.to_owned();
    let claims = value["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .unwrap_or(uhs)
        .to_owned();
    Ok(Xbox { token, uhs: claims })
}

fn minecraft(xsts: &str, uhs: &str) -> Result<String, Error> {
    let body = json!({
        "identityToken": format!("XBL3.0 x={uhs};{xsts}")
    });
    let value = post_json(MINECRAFT, &body)?;
    value["access_token"]
        .as_str()
        .map(str::to_owned)
        .ok_or(Error::NoProfile)
}

fn profile(access_token: &str) -> Result<Profile, Error> {
    let response = ureq::get(PROFILE)
        .timeout(Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(|error| Error::Http(error.to_string()))?;
    let text = response
        .into_string()
        .map_err(|error| Error::Http(error.to_string()))?;
    serde_json::from_str(&text).map_err(Error::Json)
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<String, Error> {
    let response = ureq::post(url)
        .timeout(Duration::from_secs(30))
        .send_form(form)
        .map_err(|error| Error::Http(error.to_string()))?;
    response
        .into_string()
        .map_err(|error| Error::Http(error.to_string()))
}

fn post_json(url: &str, body: &Value) -> Result<Value, Error> {
    let response = ureq::post(url)
        .timeout(Duration::from_secs(30))
        .send_json(body)
        .map_err(|error| Error::Http(error.to_string()))?;
    let text = response
        .into_string()
        .map_err(|error| Error::Http(error.to_string()))?;
    serde_json::from_str(&text).map_err(Error::Json)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: String,
}

struct Xbox {
    token: String,
    uhs: String,
}

#[cfg(test)]
mod tests {
    use super::{DeviceCode, Profile};

    #[test]
    fn parses_device_code() {
        let code: DeviceCode = serde_json::from_str(
            r#"{"device_code":"d","user_code":"u","verification_uri":"https://example.com","expires_in":900,"interval":5,"message":"m"}"#,
        )
        .expect("device code");
        assert_eq!(code.user_code, "u");
        assert_eq!(code.interval, 5);
    }

    #[test]
    fn parses_refresh_token_response() {
        let token: super::TokenResponse =
            serde_json::from_str(r#"{"access_token":"a","refresh_token":"r","expires_in":3600}"#)
                .expect("token");
        assert_eq!(token.access_token, "a");
        assert_eq!(token.refresh_token.as_deref(), Some("r"));
        assert_eq!(token.expires_in, Some(3600));
    }

    #[test]
    fn parses_profile() {
        let profile: Profile =
            serde_json::from_str(r#"{"id":"uuid","name":"Player"}"#).expect("profile");
        assert_eq!(profile.name, "Player");
    }
}

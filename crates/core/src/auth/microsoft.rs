use std::time::Duration;

use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use crate::launch::{AuthInfo, USER_TYPE_MSA};

#[cfg(not(debug_assertions))]
const DEFAULT_CLIENT_ID: &str = "";
const SCOPE: &str = "XboxLive.signin offline_access";
#[cfg(debug_assertions)]
const DEBUG_MINECRAFT_CLIENT_ID: &str = "00000000402b5328";
const LEGACY_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const LEGACY_DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const LEGACY_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";

pub fn client_id() -> String {
    let configured = std::env::var("HMCL_RS_MICROSOFT_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            option_env!("HMCL_RS_MICROSOFT_CLIENT_ID")
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        });
    if let Some(configured) = configured {
        return configured;
    }
    #[cfg(debug_assertions)]
    return DEBUG_MINECRAFT_CLIENT_ID.to_string();
    #[cfg(not(debug_assertions))]
    DEFAULT_CLIENT_ID.to_string()
}

fn legacy_live_flow(client_id: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        client_id.eq_ignore_ascii_case(DEBUG_MINECRAFT_CLIENT_ID)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = client_id;
        false
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
    #[serde(default)]
    pub message: String,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftSession {
    pub profile_name: String,
    pub profile_id: String,
    pub token_type: String,
    pub access_token: String,
    pub refresh_token: String,
    pub not_after: i64,
    #[serde(default)]
    pub userid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin_url: Option<String>,
}

impl MicrosoftSession {
    pub fn auth_info(&self) -> AuthInfo {
        AuthInfo {
            username: self.profile_name.clone(),
            uuid: Uuid::parse_str(&self.profile_id)
                .expect("stored Microsoft sessions are created from a validated profile UUID"),
            access_token: self.access_token.clone(),
            user_type: USER_TYPE_MSA.to_string(),
            user_properties: "{}".to_string(),
            launch_arguments: None,
        }
    }

    pub fn needs_refresh(&self) -> bool {
        chrono::Utc::now().timestamp_millis() + 60_000 >= self.not_after
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MicrosoftAuthError {
    #[error("尚未配置 Microsoft Client ID")]
    MissingClientId,
    #[error("微软登录已过期，请重试")]
    DeviceCodeExpired,
    #[error("微软登录已被取消")]
    AuthorizationDeclined,
    #[error("此微软账户没有 Xbox 账户")]
    MissingXboxAccount,
    #[error("儿童账户需要先加入微软家庭")]
    ChildAccount,
    #[error("此微软账户所在地区暂不支持 Xbox Live")]
    CountryUnavailable,
    #[error("此 Xbox 账户已被封禁")]
    XboxBanned,
    #[error("此微软账户没有 Minecraft: Java Edition")]
    NoMinecraftLicense,
    #[error("此微软账户尚未创建 Minecraft Java 版角色")]
    NoMinecraftProfile,
    #[error("{service} 返回错误 {status}: {message}")]
    Remote {
        service: &'static str,
        status: u16,
        message: String,
    },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("服务器返回了无效的 Minecraft UUID: {0}")]
    InvalidProfileId(String),
}

pub async fn request_device_code(
    client: &Client,
    client_id: &str,
) -> Result<DeviceCode, MicrosoftAuthError> {
    if client_id.trim().is_empty() {
        return Err(MicrosoftAuthError::MissingClientId);
    }
    let legacy = legacy_live_flow(client_id);
    let mut form = vec![
        ("client_id", client_id),
        ("scope", if legacy { LEGACY_SCOPE } else { SCOPE }),
    ];
    if legacy {
        form.push(("response_type", "device_code"));
    }
    send_json(
        client
            .post(if legacy {
                LEGACY_DEVICE_CODE_URL
            } else {
                DEVICE_CODE_URL
            })
            .form(&form),
        "Microsoft OAuth",
    )
    .await
}

pub async fn authenticate_device_code(
    client: &Client,
    client_id: &str,
    code: &DeviceCode,
    mut on_authorized: impl FnMut(),
) -> Result<MicrosoftSession, MicrosoftAuthError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(1);
    let legacy = legacy_live_flow(client_id);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(MicrosoftAuthError::DeviceCodeExpired);
        }

        let response = client
            .post(if legacy { LEGACY_TOKEN_URL } else { TOKEN_URL })
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
            ])
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if status.is_success() {
            let token: OAuthToken = decode_json("Microsoft OAuth", status, &body)?;
            on_authorized();
            return authenticate_live_token(client, token, legacy).await;
        }

        let error = serde_json::from_str::<OAuthError>(&body).unwrap_or_default();
        match error.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "authorization_declined" => return Err(MicrosoftAuthError::AuthorizationDeclined),
            "expired_token" | "bad_verification_code" => {
                return Err(MicrosoftAuthError::DeviceCodeExpired)
            }
            _ => {
                return Err(MicrosoftAuthError::Remote {
                    service: "Microsoft OAuth",
                    status: status.as_u16(),
                    message: remote_message(&body),
                })
            }
        }
    }
}

pub async fn refresh(
    client: &Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<MicrosoftSession, MicrosoftAuthError> {
    if client_id.trim().is_empty() {
        return Err(MicrosoftAuthError::MissingClientId);
    }
    let legacy = legacy_live_flow(client_id);
    let mut token: OAuthToken = send_json(
        client
            .post(if legacy { LEGACY_TOKEN_URL } else { TOKEN_URL })
            .form(&[
                ("client_id", client_id),
                ("scope", if legacy { LEGACY_SCOPE } else { SCOPE }),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ]),
        "Microsoft OAuth",
    )
    .await?;
    if token.refresh_token.is_empty() {
        token.refresh_token = refresh_token.to_string();
    }
    authenticate_live_token(client, token, legacy).await
}

async fn authenticate_live_token(
    client: &Client,
    live: OAuthToken,
    legacy_live: bool,
) -> Result<MicrosoftSession, MicrosoftAuthError> {
    let rps_ticket = if legacy_live {
        live.access_token.clone()
    } else {
        format!("d={}", live.access_token)
    };
    let xbox: XboxResponse = send_json(
        client
            .post("https://user.auth.xboxlive.com/user/authenticate")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(
                serde_json::json!({
                    "Properties": {
                        "AuthMethod": "RPS",
                        "SiteName": "user.auth.xboxlive.com",
                        "RpsTicket": rps_ticket
                    },
                    "RelyingParty": "http://auth.xboxlive.com",
                    "TokenType": "JWT"
                })
                .to_string(),
            ),
        "Xbox Live",
    )
    .await?;
    let uhs = xbox.user_hash()?;

    let xsts_response = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "Properties": {
                    "SandboxId": "RETAIL",
                    "UserTokens": [xbox.token]
                },
                "RelyingParty": "rp://api.minecraftservices.com/",
                "TokenType": "JWT"
            })
            .to_string(),
        )
        .send()
        .await?;
    let xsts_status = xsts_response.status();
    let xsts_body = xsts_response.text().await?;
    let xsts: XboxResponse =
        serde_json::from_str(&xsts_body).map_err(|error| MicrosoftAuthError::Remote {
            service: "Xbox XSTS",
            status: xsts_status.as_u16(),
            message: format!("响应格式错误: {error}"),
        })?;
    if !xsts_status.is_success() || xsts.xerr != 0 {
        return Err(xbox_error(xsts.xerr, xsts_status, &xsts_body));
    }
    if xsts.user_hash()? != uhs {
        return Err(MicrosoftAuthError::Remote {
            service: "Xbox XSTS",
            status: xsts_status.as_u16(),
            message: "用户标识不一致".to_string(),
        });
    }

    let minecraft: MinecraftToken = send_json(
        client
            .post("https://api.minecraftservices.com/authentication/login_with_xbox")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(
                serde_json::json!({
                    "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token)
                })
                .to_string(),
            ),
        "Minecraft",
    )
    .await?;

    let entitlement = client
        .get("https://api.minecraftservices.com/entitlements/mcstore")
        .bearer_auth(&minecraft.access_token)
        .send()
        .await?;
    if !entitlement.status().is_success() {
        return Err(MicrosoftAuthError::NoMinecraftLicense);
    }

    let profile_response = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&minecraft.access_token)
        .send()
        .await?;
    if profile_response.status() == StatusCode::NOT_FOUND {
        let license: MinecraftLicense = send_json(
            client
                .get("https://api.minecraftservices.com/entitlements/license")
                .bearer_auth(&minecraft.access_token),
            "Minecraft License",
        )
        .await?;
        return Err(
            if license
                .items
                .iter()
                .any(|item| item.name == "game_minecraft")
            {
                MicrosoftAuthError::NoMinecraftProfile
            } else {
                MicrosoftAuthError::NoMinecraftLicense
            },
        );
    }
    let profile_status = profile_response.status();
    let profile_body = profile_response.text().await?;
    let profile: MinecraftProfile =
        decode_json("Minecraft Profile", profile_status, &profile_body)?;
    let profile_id = Uuid::parse_str(&profile.id)
        .map_err(|_| MicrosoftAuthError::InvalidProfileId(profile.id.clone()))?;

    Ok(MicrosoftSession {
        profile_name: profile.name,
        profile_id: profile_id.to_string(),
        token_type: if minecraft.token_type.is_empty() {
            "Bearer".to_string()
        } else {
            minecraft.token_type
        },
        access_token: minecraft.access_token,
        refresh_token: live.refresh_token,
        not_after: chrono::Utc::now().timestamp_millis() + i64::from(minecraft.expires_in) * 1000,
        userid: minecraft.username,
        skin_url: profile.skins.first().map(|skin| skin.url.clone()),
    })
}

async fn send_json<T: DeserializeOwned>(
    request: RequestBuilder,
    service: &'static str,
) -> Result<T, MicrosoftAuthError> {
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await?;
    decode_json(service, status, &body)
}

fn decode_json<T: DeserializeOwned>(
    service: &'static str,
    status: StatusCode,
    body: &str,
) -> Result<T, MicrosoftAuthError> {
    if !status.is_success() {
        return Err(MicrosoftAuthError::Remote {
            service,
            status: status.as_u16(),
            message: remote_message(body),
        });
    }
    serde_json::from_str(body).map_err(|error| MicrosoftAuthError::Remote {
        service,
        status: status.as_u16(),
        message: format!("响应格式错误: {error}"),
    })
}

fn remote_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.chars().take(300).collect();
    };
    [
        "error_description",
        "errorMessage",
        "Message",
        "message",
        "error",
    ]
    .iter()
    .find_map(|key| value.get(key).and_then(|item| item.as_str()))
    .unwrap_or("未知错误")
    .to_string()
}

fn xbox_error(xerr: u64, status: StatusCode, body: &str) -> MicrosoftAuthError {
    match xerr {
        2_148_916_227 => MicrosoftAuthError::XboxBanned,
        2_148_916_233 => MicrosoftAuthError::MissingXboxAccount,
        2_148_916_235 => MicrosoftAuthError::CountryUnavailable,
        2_148_916_238 => MicrosoftAuthError::ChildAccount,
        _ => MicrosoftAuthError::Remote {
            service: "Xbox XSTS",
            status: status.as_u16(),
            message: remote_message(body),
        },
    }
}

#[derive(Debug, Deserialize)]
struct OAuthToken {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
}

#[derive(Debug, Default, Deserialize)]
struct OAuthError {
    #[serde(default)]
    error: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxResponse {
    #[serde(default)]
    token: String,
    #[serde(default, rename = "XErr")]
    xerr: u64,
    #[serde(default)]
    display_claims: XboxDisplayClaims,
}

impl XboxResponse {
    fn user_hash(&self) -> Result<String, MicrosoftAuthError> {
        self.display_claims
            .xui
            .first()
            .and_then(|claim| claim.uhs.clone())
            .ok_or_else(|| MicrosoftAuthError::Remote {
                service: "Xbox Live",
                status: 200,
                message: "响应中没有用户标识".to_string(),
            })
    }
}

#[derive(Debug, Default, Deserialize)]
struct XboxDisplayClaims {
    #[serde(default)]
    xui: Vec<XboxClaim>,
}

#[derive(Debug, Default, Deserialize)]
struct XboxClaim {
    #[serde(default)]
    uhs: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MinecraftToken {
    #[serde(default)]
    username: String,
    access_token: String,
    #[serde(default)]
    token_type: String,
    expires_in: i32,
}

#[derive(Debug, Deserialize)]
struct MinecraftProfile {
    id: String,
    name: String,
    #[serde(default)]
    skins: Vec<MinecraftSkin>,
}

#[derive(Debug, Deserialize)]
struct MinecraftSkin {
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct MinecraftLicense {
    #[serde(default)]
    items: Vec<MinecraftLicenseItem>,
}

#[derive(Debug, Deserialize)]
struct MinecraftLicenseItem {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox_error_codes_have_actionable_messages() {
        assert!(matches!(
            xbox_error(2_148_916_238, StatusCode::UNAUTHORIZED, "{}"),
            MicrosoftAuthError::ChildAccount
        ));
        assert!(matches!(
            xbox_error(2_148_916_233, StatusCode::UNAUTHORIZED, "{}"),
            MicrosoftAuthError::MissingXboxAccount
        ));
    }

    #[test]
    fn compact_minecraft_profile_ids_are_valid_uuids() {
        let id = "853c80ef3c3749fdaa49938b674adae6";
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[test]
    fn minecraft_profile_keeps_the_official_skin_url() {
        let profile: MinecraftProfile = serde_json::from_str(
            r#"{"id":"853c80ef3c3749fdaa49938b674adae6","name":"Alex","skins":[{"url":"https://textures.minecraft.net/texture/example"}]}"#,
        )
        .unwrap();
        assert_eq!(
            profile.skins.first().map(|skin| skin.url.as_str()),
            Some("https://textures.minecraft.net/texture/example")
        );
    }
}

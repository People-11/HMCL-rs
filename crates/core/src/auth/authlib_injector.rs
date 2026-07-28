use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use reqwest::{Client, StatusCode, Url};
use ring::digest::{digest, SHA256};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use crate::download::DownloadProvider;
use crate::launch::{AuthInfo, USER_TYPE_MSA};
use crate::version::{Argument, Arguments};

const LATEST_BUILD_URL: &str = "https://authlib-injector.yushi.moe/artifact/latest.json";
const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthlibInjectorServer {
    pub url: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    #[serde(default)]
    pub non_email_login: bool,
    #[serde(default)]
    pub metadata: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameProfile {
    pub id: String,
    pub name: String,
}

impl GameProfile {
    pub fn uuid(&self) -> Result<Uuid, AuthlibInjectorError> {
        Uuid::parse_str(&self.id)
            .or_else(|_| Uuid::parse_str(&hyphenate_uuid(&self.id)))
            .map_err(|_| AuthlibInjectorError::InvalidProfileId(self.id.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthlibInjectorSession {
    pub client_token: String,
    pub access_token: String,
    pub selected_profile: Option<GameProfile>,
    #[serde(default)]
    pub available_profiles: Vec<GameProfile>,
    #[serde(default)]
    pub user_properties: BTreeMap<String, String>,
}

impl AuthlibInjectorSession {
    pub fn auth_info(
        &self,
        server: &AuthlibInjectorServer,
        artifact: &Path,
    ) -> Result<AuthInfo, AuthlibInjectorError> {
        let profile = self
            .selected_profile
            .as_ref()
            .ok_or(AuthlibInjectorError::NoSelectedProfile)?;
        let properties = self
            .user_properties
            .iter()
            .map(|(key, value)| (key.clone(), vec![value.clone()]))
            .collect::<BTreeMap<_, _>>();
        let prefetched = base64::engine::general_purpose::STANDARD.encode(&server.metadata);
        Ok(AuthInfo {
            username: profile.name.clone(),
            uuid: profile.uuid()?,
            access_token: self.access_token.clone(),
            user_type: USER_TYPE_MSA.to_string(),
            user_properties: serde_json::to_string(&properties)
                .expect("string-only user properties always serialize"),
            launch_arguments: Some(Arguments {
                game: None,
                jvm: Some(
                    [
                        format!("-javaagent:{}={}", artifact.display(), server.url),
                        "-Dauthlibinjector.side=client".to_string(),
                        format!("-Dauthlibinjector.yggdrasil.prefetched={prefetched}"),
                    ]
                    .into_iter()
                    .map(Argument::Plain)
                    .collect(),
                ),
            }),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthlibInjectorError {
    #[error("认证服务器地址无效: {0}")]
    InvalidUrl(String),
    #[error("认证服务器返回的元数据无效")]
    InvalidMetadata,
    #[error("认证服务器返回内容过大")]
    ResponseTooLarge,
    #[error("用户名、密码或角色无效")]
    InvalidCredentials,
    #[error("认证服务器返回错误 {status}: {message}")]
    Remote { status: u16, message: String },
    #[error("认证服务器改变了客户端令牌")]
    ClientTokenChanged,
    #[error("该账户没有可用角色")]
    NoSelectedProfile,
    #[error("认证服务器返回了无效的角色 UUID: {0}")]
    InvalidProfileId(String),
    #[error("authlib-injector 下载信息缺少 SHA-256 校验值")]
    MissingChecksum,
    #[error("authlib-injector 校验失败")]
    ChecksumMismatch,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub async fn locate_server(
    client: &Client,
    input: &str,
) -> Result<AuthlibInjectorServer, AuthlibInjectorError> {
    let input = input.trim();
    let initial = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let mut url =
        Url::parse(&initial).map_err(|_| AuthlibInjectorError::InvalidUrl(input.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AuthlibInjectorError::InvalidUrl(input.to_string()));
    }

    let mut response = client.get(url.clone()).send().await?.error_for_status()?;
    url = response.url().clone();
    if let Some(location) = response
        .headers()
        .get("x-authlib-injector-api-location")
        .and_then(|value| value.to_str().ok())
    {
        let located = url
            .join(location)
            .map_err(|_| AuthlibInjectorError::InvalidUrl(location.to_string()))?;
        if normalized_url(&located) != normalized_url(&url) {
            response = client
                .get(located.clone())
                .send()
                .await?
                .error_for_status()?;
            url = located;
        }
    }
    let metadata = limited_text(response, MAX_METADATA_BYTES).await?;
    let value: serde_json::Value =
        serde_json::from_str(&metadata).map_err(|_| AuthlibInjectorError::InvalidMetadata)?;
    let object = value
        .as_object()
        .ok_or(AuthlibInjectorError::InvalidMetadata)?;
    let meta = object.get("meta").and_then(serde_json::Value::as_object);
    let name = meta
        .and_then(|meta| meta.get("serverName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| url.as_str())
        .to_string();
    let links = meta
        .and_then(|meta| meta.get("links"))
        .and_then(serde_json::Value::as_object)
        .map(|links| {
            links
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let non_email_login = meta
        .and_then(|meta| meta.get("feature.non_email_login"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    Ok(AuthlibInjectorServer {
        url: normalized_url(&url),
        name,
        links,
        non_email_login,
        metadata,
    })
}

pub async fn authenticate(
    client: &Client,
    server: &AuthlibInjectorServer,
    username: &str,
    password: &str,
) -> Result<AuthlibInjectorSession, AuthlibInjectorError> {
    let client_token = Uuid::now_v7().simple().to_string();
    let payload = serde_json::json!({
        "agent": {"name": "Minecraft", "version": 1},
        "username": username,
        "password": password,
        "clientToken": client_token,
        "requestUser": true
    });
    let response: AuthenticationResponse = post_json(
        client,
        endpoint(server, "authserver/authenticate")?,
        &payload,
    )
    .await?;
    response.into_session(&client_token)
}

pub async fn refresh(
    client: &Client,
    server: &AuthlibInjectorServer,
    session: &AuthlibInjectorSession,
    selected_profile: Option<&GameProfile>,
) -> Result<AuthlibInjectorSession, AuthlibInjectorError> {
    let mut payload = serde_json::json!({
        "accessToken": session.access_token,
        "clientToken": session.client_token,
        "requestUser": true
    });
    if let Some(profile) = selected_profile {
        payload["selectedProfile"] = serde_json::json!({
            "id": profile.uuid()?.simple().to_string(),
            "name": profile.name
        });
    }
    let response: AuthenticationResponse =
        post_json(client, endpoint(server, "authserver/refresh")?, &payload).await?;
    let refreshed = response.into_session(&session.client_token)?;
    if let Some(profile) = selected_profile {
        if refreshed.selected_profile.as_ref().map(|value| &value.id) != Some(&profile.id)
            && refreshed
                .selected_profile
                .as_ref()
                .and_then(|value| value.uuid().ok())
                != Some(profile.uuid()?)
        {
            return Err(AuthlibInjectorError::NoSelectedProfile);
        }
    }
    Ok(refreshed)
}

pub async fn validate(
    client: &Client,
    server: &AuthlibInjectorServer,
    session: &AuthlibInjectorSession,
) -> Result<bool, AuthlibInjectorError> {
    let response = client
        .post(endpoint(server, "authserver/validate")?)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&serde_json::json!({
            "accessToken": session.access_token,
            "clientToken": session.client_token
        }))?)
        .send()
        .await?;
    if response.status().is_success() {
        return Ok(true);
    }
    if response.status() == StatusCode::FORBIDDEN {
        return Ok(false);
    }
    Err(remote_error(response).await)
}

pub async fn ensure_artifact(
    client: &Client,
    provider: &DownloadProvider,
    destination: &Path,
) -> Result<PathBuf, AuthlibInjectorError> {
    let checksum_file = destination.with_extension("jar.sha256");
    if let Ok(expected) = std::fs::read_to_string(&checksum_file) {
        if destination.is_file() && sha256_file(destination)?.eq_ignore_ascii_case(expected.trim())
        {
            return Ok(destination.to_path_buf());
        }
    }

    let latest: ArtifactVersion =
        get_candidates_json(client, &provider.inject_url_candidates(LATEST_BUILD_URL)).await?;
    let expected = latest
        .checksums
        .get("sha256")
        .ok_or(AuthlibInjectorError::MissingChecksum)?
        .to_ascii_lowercase();

    let bytes = get_candidates_bytes(
        client,
        &provider.inject_url_candidates(&latest.download_url),
        MAX_ARTIFACT_BYTES,
    )
    .await?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(AuthlibInjectorError::ChecksumMismatch);
    }
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = destination.with_extension("jar.part");
    tokio::fs::write(&temporary, &bytes).await?;
    tokio::fs::rename(&temporary, destination).await?;
    tokio::fs::write(checksum_file, expected).await?;
    Ok(destination.to_path_buf())
}

fn endpoint(server: &AuthlibInjectorServer, path: &str) -> Result<Url, AuthlibInjectorError> {
    Url::parse(&server.url)
        .and_then(|base| base.join(path))
        .map_err(|_| AuthlibInjectorError::InvalidUrl(server.url.clone()))
}

fn normalized_url(url: &Url) -> String {
    let mut value = url.as_str().to_string();
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn hyphenate_uuid(value: &str) -> String {
    if value.len() != 32 {
        return value.to_string();
    }
    format!(
        "{}-{}-{}-{}-{}",
        &value[0..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..32]
    )
}

async fn limited_text(
    response: reqwest::Response,
    limit: usize,
) -> Result<String, AuthlibInjectorError> {
    let bytes = response.bytes().await?;
    if bytes.len() > limit {
        return Err(AuthlibInjectorError::ResponseTooLarge);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| AuthlibInjectorError::InvalidMetadata)
}

async fn post_json<T: DeserializeOwned>(
    client: &Client,
    url: Url,
    payload: &serde_json::Value,
) -> Result<T, AuthlibInjectorError> {
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(payload)?)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(remote_error(response).await);
    }
    let text = limited_text(response, MAX_METADATA_BYTES).await?;
    serde_json::from_str(&text).map_err(Into::into)
}

async fn remote_error(response: reqwest::Response) -> AuthlibInjectorError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("errorMessage")
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| text.chars().take(300).collect());
    if status == StatusCode::FORBIDDEN.as_u16() {
        AuthlibInjectorError::InvalidCredentials
    } else {
        AuthlibInjectorError::Remote { status, message }
    }
}

async fn get_candidates_json<T: DeserializeOwned>(
    client: &Client,
    candidates: &[String],
) -> Result<T, AuthlibInjectorError> {
    let mut last_error = None;
    for candidate in candidates {
        match client.get(candidate).send().await {
            Ok(response) if response.status().is_success() => {
                let text = limited_text(response, MAX_METADATA_BYTES).await?;
                return serde_json::from_str(&text).map_err(Into::into);
            }
            Ok(response) => last_error = Some(remote_error(response).await),
            Err(error) => last_error = Some(error.into()),
        }
    }
    Err(last_error.unwrap_or(AuthlibInjectorError::InvalidMetadata))
}

async fn get_candidates_bytes(
    client: &Client,
    candidates: &[String],
    limit: usize,
) -> Result<Vec<u8>, AuthlibInjectorError> {
    let mut last_error = None;
    for candidate in candidates {
        match client.get(candidate).send().await {
            Ok(response) if response.status().is_success() => {
                let bytes = response.bytes().await?;
                if bytes.len() > limit {
                    return Err(AuthlibInjectorError::ResponseTooLarge);
                }
                return Ok(bytes.to_vec());
            }
            Ok(response) => last_error = Some(remote_error(response).await),
            Err(error) => last_error = Some(error.into()),
        }
    }
    Err(last_error.unwrap_or(AuthlibInjectorError::InvalidMetadata))
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Deserialize)]
struct AuthenticationResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "clientToken")]
    client_token: String,
    #[serde(rename = "selectedProfile")]
    selected_profile: Option<GameProfile>,
    #[serde(rename = "availableProfiles", default)]
    available_profiles: Vec<GameProfile>,
    user: Option<YggdrasilUser>,
}

impl AuthenticationResponse {
    fn into_session(
        self,
        expected_client_token: &str,
    ) -> Result<AuthlibInjectorSession, AuthlibInjectorError> {
        if self.client_token != expected_client_token {
            return Err(AuthlibInjectorError::ClientTokenChanged);
        }
        Ok(AuthlibInjectorSession {
            client_token: self.client_token,
            access_token: self.access_token,
            selected_profile: self.selected_profile,
            available_profiles: self.available_profiles,
            user_properties: self
                .user
                .and_then(|user| user.properties)
                .unwrap_or_default()
                .into_iter()
                .map(|property| (property.name, property.value))
                .collect(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct YggdrasilUser {
    #[serde(default)]
    properties: Option<Vec<YggdrasilProperty>>,
}

#[derive(Debug, Deserialize)]
struct YggdrasilProperty {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct ArtifactVersion {
    #[allow(dead_code)]
    #[serde(rename = "build_number")]
    build_number: i32,
    #[allow(dead_code)]
    version: String,
    download_url: String,
    checksums: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn locates_relative_api_header_and_parses_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-authlib-injector-api-location", "/api/yggdrasil/"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/yggdrasil/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"meta":{"serverName":"Test Skin","feature.non_email_login":true,"links":{"homepage":"https://example.test"}}}"#,
            ))
            .mount(&server)
            .await;

        let located = locate_server(&Client::new(), &server.uri()).await.unwrap();
        assert_eq!(located.name, "Test Skin");
        assert!(located.url.ends_with("/api/yggdrasil/"));
        assert!(located.non_email_login);
        assert_eq!(
            located.links.get("homepage").map(String::as_str),
            Some("https://example.test")
        );
    }

    #[test]
    fn compact_profile_uuid_is_accepted() {
        let profile = GameProfile {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            name: "Player".to_string(),
        };
        assert_eq!(
            profile.uuid().unwrap().to_string(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
    }

    #[test]
    fn java_agent_arguments_match_hmcl() {
        let server = AuthlibInjectorServer {
            url: "https://example.test/api/yggdrasil/".to_string(),
            name: "Example".to_string(),
            links: Default::default(),
            non_email_login: false,
            metadata: r#"{"meta":{"serverName":"Example"}}"#.to_string(),
        };
        let session = AuthlibInjectorSession {
            client_token: "client-token".to_string(),
            access_token: "access-token".to_string(),
            selected_profile: Some(GameProfile {
                id: "0123456789abcdef0123456789abcdef".to_string(),
                name: "Player".to_string(),
            }),
            available_profiles: Vec::new(),
            user_properties: BTreeMap::from([(
                "preferredLanguage".to_string(),
                "zh_CN".to_string(),
            )]),
        };

        let auth = session
            .auth_info(&server, Path::new("libraries/authlib-injector.jar"))
            .unwrap();
        let arguments = auth
            .launch_arguments
            .unwrap()
            .jvm
            .unwrap()
            .into_iter()
            .map(|argument| match argument {
                Argument::Plain(value) => value,
                Argument::Ruled { .. } => {
                    panic!("authlib-injector arguments must be unconditional")
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(auth.username, "Player");
        assert_eq!(auth.user_type, USER_TYPE_MSA);
        assert_eq!(arguments.len(), 3);
        assert!(arguments[0].starts_with("-javaagent:"));
        assert!(arguments[0].ends_with("=https://example.test/api/yggdrasil/"));
        assert_eq!(arguments[1], "-Dauthlibinjector.side=client");
        assert_eq!(
            arguments[2],
            format!(
                "-Dauthlibinjector.yggdrasil.prefetched={}",
                base64::engine::general_purpose::STANDARD.encode(server.metadata)
            )
        );
    }

    #[tokio::test]
    async fn refresh_selects_profile_and_validate_accepts_session() {
        let mock = MockServer::start().await;
        let server = AuthlibInjectorServer {
            url: format!("{}/", mock.uri()),
            name: "Test".to_string(),
            links: Default::default(),
            non_email_login: true,
            metadata: "{}".to_string(),
        };
        let profile = GameProfile {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            name: "Player".to_string(),
        };
        let session = AuthlibInjectorSession {
            client_token: "client-token".to_string(),
            access_token: "old-token".to_string(),
            selected_profile: None,
            available_profiles: vec![profile.clone()],
            user_properties: Default::default(),
        };

        Mock::given(method("POST"))
            .and(path("/authserver/refresh"))
            .and(body_json(serde_json::json!({
                "accessToken": "old-token",
                "clientToken": "client-token",
                "requestUser": true,
                "selectedProfile": {
                    "id": "0123456789abcdef0123456789abcdef",
                    "name": "Player"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "new-token",
                "clientToken": "client-token",
                "selectedProfile": {
                    "id": "0123456789abcdef0123456789abcdef",
                    "name": "Player"
                },
                "availableProfiles": []
            })))
            .mount(&mock)
            .await;

        let refreshed = refresh(&Client::new(), &server, &session, Some(&profile))
            .await
            .unwrap();
        assert_eq!(refreshed.access_token, "new-token");
        assert_eq!(refreshed.selected_profile, Some(profile));

        Mock::given(method("POST"))
            .and(path("/authserver/validate"))
            .and(body_json(serde_json::json!({
                "accessToken": "new-token",
                "clientToken": "client-token"
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock)
            .await;
        assert!(validate(&Client::new(), &server, &refreshed).await.unwrap());
    }

    #[tokio::test]
    async fn verified_cached_artifact_does_not_need_the_network() {
        let directory = std::env::temp_dir().join(format!(
            "hmcl-rs-authlib-injector-{}",
            Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let artifact = directory.join("authlib-injector.jar");
        let bytes = b"already verified";
        std::fs::write(&artifact, bytes).unwrap();
        std::fs::write(artifact.with_extension("jar.sha256"), sha256_hex(bytes)).unwrap();

        let result = ensure_artifact(&Client::new(), &DownloadProvider::mojang(), &artifact)
            .await
            .unwrap();
        assert_eq!(result, artifact);

        std::fs::remove_dir_all(directory).unwrap();
    }
}

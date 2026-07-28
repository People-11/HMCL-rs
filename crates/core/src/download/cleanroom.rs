use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use zip::ZipArchive;

use crate::download::{CacheRepository, DownloadProvider};
use crate::install::GameRepository;
use crate::version::Version;

use super::forge::{self, ForgeInstallError};

pub const PATCH_ID: &str = "cleanroom";

#[derive(Debug, thiserror::Error)]
pub enum CleanroomInstallError {
    #[error(transparent)]
    Forge(#[from] ForgeInstallError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("failed to parse install_profile.json header: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "this is not a cleanroom installer (install_profile.json's \"profile\" field is {0:?})"
    )]
    NotACleanroomInstaller(Option<String>),
    #[error("this installer targets Minecraft {expected}, but {actual} was requested")]
    VersionMismatch { expected: String, actual: String },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("no cleanroom build available for {0}")]
    NoBuildForGameVersion(String),
    #[error(transparent)]
    Fetch(#[from] crate::download::FetchError),
}

#[derive(Debug, Deserialize)]
struct ProfileHeader {
    profile: Option<String>,
    minecraft: String,
    version: String,
}

fn read_profile_header(installer_jar: &Path) -> Result<ProfileHeader, CleanroomInstallError> {
    let file = std::fs::File::open(installer_jar)?;
    let mut archive = ZipArchive::new(file)?;
    let mut text = String::new();
    archive
        .by_name("install_profile.json")?
        .read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

fn modify_version(version: &str) -> String {
    version.replace("cleanroom-", "")
}

pub async fn install_cleanroom(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    installer_jar: &Path,
    vanilla_version: &Version,
    java_binary: &Path,
) -> Result<Version, CleanroomInstallError> {
    let header = read_profile_header(installer_jar)?;
    if !header
        .profile
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID))
    {
        return Err(CleanroomInstallError::NotACleanroomInstaller(
            header.profile,
        ));
    }
    if header.minecraft != vanilla_version.id {
        return Err(CleanroomInstallError::VersionMismatch {
            expected: header.minecraft,
            actual: vanilla_version.id.clone(),
        });
    }

    let self_version = modify_version(&header.version);
    let patch = forge::install_new_forge(
        client,
        provider,
        cache,
        repo,
        installer_jar,
        vanilla_version,
        java_binary,
        PATCH_ID,
        &self_version,
    )
    .await?;
    Ok(patch)
}

// ============================================================================
// 构建发现：对应 HMCL-java `download/cleanroom/CleanroomVersionList.java`。
//
// Cleanroom 只支持 Minecraft 1.12.2（Java 版把这个游戏版本号硬编码进了
// `CleanroomVersionList.refreshAsync`，不是从 API 响应里读出来的），版本清单走
// 一个 HMCL 项目自己维护的元数据镜像（代理 Cleanroom 真实的 GitHub Releases），
// 真实抓取过：`https://hmcl.glavo.site/metadata/cleanroom/index.json` 返回
// `[{"name": "0.6.6-alpha", "created_at": "..."}]`，数组顺序本身就是按发布时间
// 从新到旧（最新的在最前面），装的时候拼
// `https://hmcl.glavo.site/metadata/cleanroom/files/cleanroom-{name}-installer.jar`。
// ============================================================================

const CLEANROOM_INDEX_URL: &str = "https://hmcl.glavo.site/metadata/cleanroom/index.json";
const CLEANROOM_ONLY_GAME_VERSION: &str = "1.12.2";

#[derive(Debug, Clone, Deserialize)]
struct CleanroomRelease {
    name: String,
    #[allow(dead_code)] // 只用来在真实数据里核对/调试, 挑构建时目前不用它排序(API 顺序本来就对)。
    created_at: String,
}

#[derive(Debug, Clone)]
pub struct CleanroomBuild {
    pub version: String,
    pub installer_url: String,
}

fn release_to_build(r: CleanroomRelease) -> CleanroomBuild {
    let installer_url = format!(
        "https://hmcl.glavo.site/metadata/cleanroom/files/cleanroom-{}-installer.jar",
        r.name
    );
    CleanroomBuild {
        version: r.name,
        installer_url,
    }
}

async fn fetch_releases(
    client: &reqwest::Client,
    index_url: &str,
) -> Result<Vec<CleanroomRelease>, CleanroomInstallError> {
    let text = client
        .get(index_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(serde_json::from_str(&text)?)
}

/// 返回 Cleanroom 所有可用构建，最新的在最前面（API 原始顺序）。`game_version`
/// 只用来校验调用方要的是不是 `1.12.2`——Cleanroom 目前没有其它游戏版本可选,
/// 传别的版本号直接返回空列表(照抄 Java 版 `CleanroomVersionList` 把所有构建都
/// 硬塞进 "1.12.2" 这一个桶、其它游戏版本查不到任何东西的效果)。
pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    game_version: &str,
) -> Result<Vec<CleanroomBuild>, CleanroomInstallError> {
    if game_version != CLEANROOM_ONLY_GAME_VERSION {
        return Ok(Vec::new());
    }
    let releases = fetch_releases(client, CLEANROOM_INDEX_URL).await?;
    Ok(releases.into_iter().map(release_to_build).collect())
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    game_version: &str,
) -> Result<CleanroomBuild, CleanroomInstallError> {
    let mut builds = fetch_compatible_builds(client, game_version).await?;
    if builds.is_empty() {
        return Err(CleanroomInstallError::NoBuildForGameVersion(
            game_version.to_string(),
        ));
    }
    Ok(builds.remove(0))
}

pub async fn fetch_build_by_version(
    client: &reqwest::Client,
    game_version: &str,
    version: &str,
) -> Result<CleanroomBuild, CleanroomInstallError> {
    fetch_compatible_builds(client, game_version)
        .await?
        .into_iter()
        .find(|b| b.version == version)
        .ok_or_else(|| {
            CleanroomInstallError::NoBuildForGameVersion(format!("{version} for {game_version}"))
        })
}

pub async fn download_installer(
    client: &reqwest::Client,
    build: &CleanroomBuild,
    dest: &Path,
) -> Result<(), CleanroomInstallError> {
    crate::download::fetch_to_file(
        client,
        std::slice::from_ref(&build.installer_url),
        dest,
        &crate::download::Expected::default(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE_INDEX: &str = r#"[
        {"name": "0.6.6-alpha", "created_at": "2026-07-24T13:32:08Z"},
        {"name": "0.6.5-alpha", "created_at": "2026-07-24T01:18:49Z"}
    ]"#;

    #[tokio::test]
    async fn fetch_releases_parses_real_shaped_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_INDEX))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let releases = fetch_releases(&client, &format!("{}/index.json", server.uri()))
            .await
            .unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].name, "0.6.6-alpha");
    }

    #[tokio::test]
    async fn fetch_compatible_builds_short_circuits_for_any_game_version_other_than_1_12_2() {
        let client = reqwest::Client::new();
        let builds = fetch_compatible_builds(&client, "1.20.1").await.unwrap();
        assert!(builds.is_empty(), "cleanroom only ever supports 1.12.2");
    }

    #[test]
    fn release_to_build_constructs_the_real_download_url_pattern() {
        let build = release_to_build(CleanroomRelease {
            name: "0.6.6-alpha".to_string(),
            created_at: "irrelevant".to_string(),
        });
        assert_eq!(
            build.installer_url,
            "https://hmcl.glavo.site/metadata/cleanroom/files/cleanroom-0.6.6-alpha-installer.jar"
        );
    }

    #[test]
    fn modify_version_strips_the_cleanroom_prefix_literal() {
        assert_eq!(
            modify_version("1.12.2-cleanroom-0.6.6-alpha"),
            "1.12.2-0.6.6-alpha"
        );
        assert_eq!(modify_version("no-prefix-here"), "no-prefix-here");
    }

    #[test]
    fn modify_version_is_a_case_sensitive_noop_on_real_capitalized_data() {
        assert_eq!(
            modify_version("1.12.2-Cleanroom-0.6.6-alpha"),
            "1.12.2-Cleanroom-0.6.6-alpha"
        );
    }

    #[test]
    fn profile_field_check_is_case_insensitive() {
        assert!(Some("Cleanroom").is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID)));
        assert!(Some("cleanroom").is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID)));
        assert!(Some("CLEANROOM").is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID)));
        assert!(!Some("forge").is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID)));
    }

    #[test]
    fn profile_header_shape_matches_a_real_cleanroom_installer() {
        const SAMPLE: &str = r#"{
            "spec": 0,
            "profile": "Cleanroom",
            "version": "1.12.2-Cleanroom-0.6.6-alpha",
            "json": "/version.json",
            "path": "com.cleanroommc:cleanroom:0.6.6-alpha",
            "minecraft": "1.12.2",
            "data": {},
            "processors": [],
            "libraries": [
                {"name": "com.cleanroommc:cleanroom:0.6.6-alpha", "downloads": {"artifact": {"path": "com/cleanroommc/cleanroom/0.6.6-alpha/cleanroom-0.6.6-alpha.jar", "url": "", "sha1": "85de78c74744e9439a63655e1123d967234fb0f2", "size": 6623719}}}
            ]
        }"#;
        let header: ProfileHeader = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(header.profile.as_deref(), Some("Cleanroom"));
        assert_eq!(header.minecraft, "1.12.2");
        assert!(header.profile.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(PATCH_ID)), "must pass our case-insensitive profile check despite the capitalized \"profile\" field");
    }
}

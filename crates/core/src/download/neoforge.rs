use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use zip::ZipArchive;

use crate::download::{CacheRepository, DownloadProvider};
use crate::install::GameRepository;
use crate::version::Version;

use super::forge::{self, ForgeInstallError};

pub const PATCH_ID: &str = "neoforge";

#[derive(Debug, thiserror::Error)]
pub enum NeoForgeInstallError {
    #[error(transparent)]
    Forge(#[from] ForgeInstallError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("failed to parse install_profile.json header: {0}")]
    Json(#[from] serde_json::Error),
    #[error("install_profile.json has an unrecognized \"profile\" field: {0:?}")]
    UnrecognizedProfile(Option<String>),
    #[error("this installer targets Minecraft {expected}, but {actual} was requested")]
    VersionMismatch { expected: String, actual: String },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Deserialize)]
struct ProfileHeader {
    profile: Option<String>,
    minecraft: String,
    version: String,
}

fn remove_prefix(s: &str, prefix: &str) -> String {
    s.strip_prefix(prefix).unwrap_or(s).to_string()
}

fn remove_suffix(s: &str, suffix: &str) -> String {
    s.strip_suffix(suffix).unwrap_or(s).to_string()
}

fn modify_old_style_version(game_version: &str, version: &str) -> String {
    let v = version.replace(game_version, "");
    let v = v.trim();
    let v = remove_prefix(v, "-");
    let v = remove_suffix(&v, "-");
    let v = remove_prefix(&v, "_");
    remove_suffix(&v, "_")
}

fn modify_new_style_version(version: &str) -> String {
    remove_prefix(&version.replace("neoforge", ""), "-")
}

fn read_profile_header(
    installer_jar: &Path,
) -> Result<(ProfileHeader, bool), NeoForgeInstallError> {
    let file = std::fs::File::open(installer_jar)?;
    let mut archive = ZipArchive::new(file)?;
    let mut text = String::new();
    archive
        .by_name("install_profile.json")?
        .read_to_string(&mut text)?;
    let has_neoforge_signature = archive.by_name("META-INF/NEOFORGE.RSA").is_ok();
    let header: ProfileHeader = serde_json::from_str(&text)?;
    let is_disguised_as_forge = has_neoforge_signature || text.contains("neoforge");
    Ok((header, is_disguised_as_forge))
}

pub async fn install_neoforge(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    installer_jar: &Path,
    vanilla_version: &Version,
    java_binary: &Path,
) -> Result<Version, NeoForgeInstallError> {
    let (header, is_disguised_as_forge) = read_profile_header(installer_jar)?;

    if header.minecraft != vanilla_version.id {
        return Err(NeoForgeInstallError::VersionMismatch {
            expected: header.minecraft,
            actual: vanilla_version.id.clone(),
        });
    }

    let self_version = match header.profile.as_deref() {
        Some("forge") if is_disguised_as_forge => {
            let old_style = modify_old_style_version(&header.minecraft, &header.version);
            remove_prefix(&old_style.replace("forge", ""), "-")
        }
        Some("neoforge") | Some("NeoForge") => modify_new_style_version(&header.version),
        other => {
            return Err(NeoForgeInstallError::UnrecognizedProfile(
                other.map(str::to_string),
            ))
        }
    };

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
// 构建发现：对应 HMCL-java `download/neoforge/NeoForgeBMCLVersionList.java`。
//
// 没有搬 `NeoForgeOfficialVersionList` 那条路——它直接打 `maven.neoforged.net`
// 的官方 API 拿全量版本号列表，然后本地用一套相当绕的字符串解析猜每个版本号
// 对应哪个游戏版本（NeoForge 的版本号编码规则在某个大版本号门槛之后变了一次，
// Java 那边专门有一段 `if (majorVersion >= 26)` 分支处理新老两种编码方式）。
// BMCLAPI 有个更直接的专用端点：`{api_root}/neoforge/list/{游戏版本}`——服务端
// already 按游戏版本过滤好了，还顺带在 `installerPath` 字段里给出了下载路径，
// 不需要在客户端重新猜版本号编码规则。真实测过 `/neoforge/list/1.20.2`。
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct BmclNeoForgeVersion {
    version: String,
    #[serde(rename = "mcversion")]
    mc_version: String,
    #[serde(rename = "installerPath")]
    installer_path: String,
}

#[derive(Debug, Clone)]
pub struct NeoForgeBuild {
    pub mc_version: String,
    pub version: String,
    pub installer_url: String,
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<Vec<NeoForgeBuild>, NeoForgeInstallError> {
    let url = format!("{api_root}/neoforge/list/{game_version}");
    let text = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let raw: Vec<BmclNeoForgeVersion> = serde_json::from_str(&text)?;

    let mut builds: Vec<NeoForgeBuild> = raw
        .into_iter()
        .map(|v| {
            let installer_path = v.installer_path.trim_start_matches("/maven/");
            NeoForgeBuild {
                mc_version: v.mc_version,
                installer_url: format!("https://maven.neoforged.net/releases/{installer_path}"),
                version: v.version,
            }
        })
        .collect();
    builds.sort_by(|a, b| dotted_numeric_cmp(&a.version, &b.version));
    Ok(builds)
}

fn dotted_numeric_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        return match (ai.next(), bi.next()) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(xn), Ok(yn)) if xn != yn => xn.cmp(&yn),
                _ if x != y => x.cmp(y),
                _ => continue,
            },
        };
    }
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
) -> Result<NeoForgeBuild, NeoForgeInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .pop()
        .ok_or_else(|| {
            NeoForgeInstallError::UnrecognizedProfile(Some(format!(
                "no neoforge build available for {game_version}"
            )))
        })
}

pub async fn fetch_build_by_version(
    client: &reqwest::Client,
    api_root: &str,
    game_version: &str,
    version: &str,
) -> Result<NeoForgeBuild, NeoForgeInstallError> {
    fetch_compatible_builds(client, api_root, game_version)
        .await?
        .into_iter()
        .find(|b| b.version == version)
        .ok_or_else(|| {
            NeoForgeInstallError::UnrecognizedProfile(Some(format!(
                "no neoforge build {version} for {game_version}"
            )))
        })
}

pub async fn download_installer(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    build: &NeoForgeBuild,
    dest: &Path,
) -> Result<(), NeoForgeInstallError> {
    crate::download::fetch_to_file(
        client,
        &provider.inject_url_candidates(&build.installer_url),
        dest,
        &crate::download::Expected::default(),
    )
    .await
    .map_err(ForgeInstallError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn dotted_numeric_cmp_orders_by_value_not_string_length() {
        assert_eq!(
            dotted_numeric_cmp("20.2.9", "20.2.93"),
            std::cmp::Ordering::Less,
            "numeric 9 < 93, even though \"9\" < \"93\" lexicographically agrees here by luck"
        );
        assert_eq!(
            dotted_numeric_cmp("20.2.93", "20.2.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(dotted_numeric_cmp("20.2.5", "20.10.1"), std::cmp::Ordering::Less, "minor 2 < 10 numerically, would be wrong as a string compare (\"2\" > \"10\"... no wait \"2\">\"1\", still must be Less numerically)");
        assert_eq!(
            dotted_numeric_cmp("20.2.93-beta", "20.2.93"),
            std::cmp::Ordering::Greater,
            "non-numeric suffix falls back to string compare"
        );
    }

    #[tokio::test]
    async fn fetch_compatible_builds_sorts_numerically_and_builds_full_urls() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            {"rawVersion": "neoforge-20.2.9", "version": "20.2.9", "mcversion": "1.20.2", "installerPath": "/maven/net/neoforged/neoforge/20.2.9/neoforge-20.2.9-installer.jar"},
            {"rawVersion": "neoforge-20.2.93", "version": "20.2.93", "mcversion": "1.20.2", "installerPath": "/maven/net/neoforged/neoforge/20.2.93/neoforge-20.2.93-installer.jar"}
        ]);
        Mock::given(method("GET"))
            .and(path("/neoforge/list/1.20.2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let builds = fetch_compatible_builds(&client, &server.uri(), "1.20.2")
            .await
            .unwrap();

        assert_eq!(builds.len(), 2);
        assert_eq!(
            builds[0].version, "20.2.9",
            "must sort numerically, not by build id order in the response"
        );
        assert_eq!(builds[1].version, "20.2.93");
        assert_eq!(
            builds[1].installer_url,
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/20.2.93/neoforge-20.2.93-installer.jar"
        );

        let latest = fetch_latest_build(&client, &server.uri(), "1.20.2")
            .await
            .unwrap();
        assert_eq!(latest.version, "20.2.93");
    }

    #[test]
    fn modify_old_style_version_strips_game_version_and_stray_separators() {
        assert_eq!(
            modify_old_style_version("1.20.1", "1.20.1-47.1.0"),
            "47.1.0"
        );
        assert_eq!(
            modify_old_style_version("1.20.1", "1.20.1-_47.1.0_-"),
            "47.1.0"
        );
        assert_eq!(
            modify_old_style_version("1.20.1", " 1.20.1-47.1.0 "),
            "47.1.0",
            "should trim before/after stripping separators"
        );
    }

    #[test]
    fn modify_new_style_version_strips_loader_name_and_leading_dash() {
        assert_eq!(modify_new_style_version("neoforge-20.1.57"), "20.1.57");
        assert_eq!(
            modify_new_style_version("20.1.57"),
            "20.1.57",
            "no loader name present: unchanged"
        );
    }

    #[test]
    fn old_style_dance_ends_up_removing_the_word_forge_too() {
        let old_style = modify_old_style_version("1.20.1", "1.20.1-forge-47.1.0");
        let final_version = remove_prefix(&old_style.replace("forge", ""), "-");
        assert_eq!(final_version, "47.1.0");
    }
}

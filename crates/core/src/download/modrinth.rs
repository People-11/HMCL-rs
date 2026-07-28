use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::download::fetch::{fetch_to_file_cached_with_progress, Expected, FetchError};
use crate::download::{CacheRepository, DownloadProvider};
use crate::version::Version;

const API_ROOT: &str = "https://api.modrinth.com/v2";

const KNOWN_LOADERS: &[&str] = &[
    "fabric",
    "forge",
    "neoforge",
    "quilt",
    "legacyfabric",
    "liteloader",
];

#[derive(Debug, thiserror::Error)]
pub enum ModrinthError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse modrinth response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("download failed: {0}")]
    Fetch(#[from] FetchError),
    #[error("no compatible version file was found for this mod")]
    NoCompatibleVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    #[serde(default)]
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub display_categories: Vec<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub downloads: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total_hits: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    #[serde(default)]
    pub icon_url: Option<String>,
}

impl SearchResponse {
    pub fn has_more(&self) -> bool {
        self.offset + (self.hits.len() as u64) < self.total_hits
    }
}

pub async fn fetch_project(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    project_id: &str,
) -> Result<Project, ModrinthError> {
    get_json(
        client,
        provider,
        &format!("{API_ROOT}/project/{project_id}"),
        &[],
    )
    .await
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectVersion {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub date_published: String,
    #[serde(default)]
    pub files: Vec<VersionFile>,
}

pub async fn fetch_version_by_sha1(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    sha1: &str,
) -> Result<Option<ProjectVersion>, ModrinthError> {
    let url = format!("{API_ROOT}/version_file/{sha1}");
    let mut last_err = None;
    for candidate in provider.inject_url_candidates(&url) {
        match client
            .get(candidate)
            .query(&[("algorithm", "sha1")])
            .send()
            .await
        {
            Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => continue,
            Ok(response) => match response.error_for_status() {
                Ok(response) => {
                    let text = response.text().await?;
                    return Ok(Some(serde_json::from_str(&text)?));
                }
                Err(error) => last_err = Some(error),
            },
            Err(error) => last_err = Some(error),
        }
    }
    match last_err {
        Some(error) => Err(error.into()),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub hashes: FileHashes,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileHashes {
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    pub name: String,
    pub project_type: String,
}

pub fn detect_loader(resolved: &Version) -> Option<&'static str> {
    let patches = resolved.patches.as_ref()?;
    patches
        .iter()
        .find_map(|p| KNOWN_LOADERS.iter().copied().find(|slug| p.id == *slug))
}

pub fn detect_game_version(resolved: &Version) -> &str {
    resolved.jar.as_deref().unwrap_or(&resolved.id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Mod,
    ResourcePack,
    Shader,
    Modpack,
}

impl ProjectType {
    fn facet_value(self) -> &'static str {
        match self {
            ProjectType::Mod => "mod",
            ProjectType::ResourcePack => "resourcepack",
            ProjectType::Shader => "shader",
            ProjectType::Modpack => "modpack",
        }
    }
}

fn build_facets(
    project_type: ProjectType,
    game_version: Option<&str>,
    category: Option<&str>,
    loader: Option<&str>,
) -> String {
    let mut facets: Vec<Vec<String>> =
        vec![vec![format!("project_type:{}", project_type.facet_value())]];
    if let Some(v) = game_version {
        facets.push(vec![format!("versions:{v}")]);
    }
    if let Some(c) = category {
        facets.push(vec![format!("categories:{c}")]);
    }
    if let Some(l) = loader {
        facets.push(vec![format!("categories:{l}")]);
    }
    serde_json::to_string(&facets).expect("Vec<Vec<String>> always serializes")
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    url: &str,
    query: &[(&str, String)],
) -> Result<T, ModrinthError> {
    let candidates = provider.inject_url_candidates(url);
    let mut last_err: Option<ModrinthError> = None;
    for candidate in candidates {
        match client
            .get(&candidate)
            .query(query)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
        {
            Ok(resp) => match resp.text().await {
                Ok(text) => return Ok(serde_json::from_str(&text)?),
                Err(e) => last_err = Some(e.into()),
            },
            Err(e) => last_err = Some(e.into()),
        }
    }
    Err(last_err.expect("inject_url_candidates always returns at least one URL"))
}

#[allow(clippy::too_many_arguments)]
pub async fn search_projects(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    project_type: ProjectType,
    query: &str,
    game_version: Option<&str>,
    category: Option<&str>,
    loader: Option<&str>,
    index: &str,
    offset: u64,
    limit: u64,
) -> Result<SearchResponse, ModrinthError> {
    let params = [
        ("query", query.to_string()),
        (
            "facets",
            build_facets(project_type, game_version, category, loader),
        ),
        ("index", index.to_string()),
        ("offset", offset.to_string()),
        ("limit", limit.to_string()),
    ];
    get_json(client, provider, &format!("{API_ROOT}/search"), &params).await
}

pub async fn fetch_project_versions(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<ProjectVersion>, ModrinthError> {
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(v) = game_version {
        params.push(("game_versions", format!("[\"{v}\"]")));
    }
    if let Some(l) = loader {
        params.push(("loaders", format!("[\"{l}\"]")));
    }
    get_json(
        client,
        provider,
        &format!("{API_ROOT}/project/{project_id}/version"),
        &params,
    )
    .await
}

pub async fn fetch_categories(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    project_type: ProjectType,
) -> Result<Vec<String>, ModrinthError> {
    let categories: Vec<Category> =
        get_json(client, provider, &format!("{API_ROOT}/tag/category"), &[]).await?;
    Ok(categories
        .into_iter()
        .filter(|category| category.project_type == project_type.facet_value())
        .map(|category| category.name)
        .collect())
}

pub async fn install_version_file(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    version: &ProjectVersion,
    dest_dir: &Path,
) -> Result<PathBuf, ModrinthError> {
    let file = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
        .ok_or(ModrinthError::NoCompatibleVersion)?;
    install_version_file_as(client, provider, cache, version, dest_dir, &file.filename).await
}

pub async fn install_version_file_as(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    version: &ProjectVersion,
    dest_dir: &Path,
    file_name: &str,
) -> Result<PathBuf, ModrinthError> {
    install_version_file_as_with_progress(
        client,
        provider,
        cache,
        version,
        dest_dir,
        file_name,
        |_| {},
    )
    .await
}

pub async fn install_version_file_as_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    version: &ProjectVersion,
    dest_dir: &Path,
    file_name: &str,
    on_chunk: impl FnMut(u64),
) -> Result<PathBuf, ModrinthError> {
    let file = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
        .ok_or(ModrinthError::NoCompatibleVersion)?;
    let dest = dest_dir.join(file_name);
    let expected = Expected {
        sha1: file.hashes.sha1.clone(),
        size: Some(file.size).filter(|&size| size != 0),
    };
    fetch_to_file_cached_with_progress(
        client,
        cache,
        &provider.inject_url_candidates(&file.url),
        &dest,
        &expected,
        on_chunk,
    )
    .await?;
    Ok(dest)
}

/// 装最新兼容版本里标为 primary 的文件（没有任何文件标 primary 就取第一个）——
/// 没做"挑具体版本/具体文件"的子界面，先跑通一条真实端到端路径。安装目的地由
/// 调用方给（必须是已经按实例隔离解析过的 `mods/` 目录，不是共享游戏根目录）。
#[allow(clippy::too_many_arguments)]
pub async fn install_latest_compatible(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
    dest_dir: &Path,
) -> Result<PathBuf, ModrinthError> {
    let versions =
        fetch_project_versions(client, provider, project_id, game_version, loader).await?;
    let version = versions
        .iter()
        .find(|version| !version.files.is_empty())
        .ok_or(ModrinthError::NoCompatibleVersion)?;
    install_version_file(client, provider, cache, version, dest_dir).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn facets_include_project_type_and_optional_filters() {
        assert_eq!(
            build_facets(ProjectType::Mod, None, None, None),
            r#"[["project_type:mod"]]"#
        );
        assert_eq!(
            build_facets(
                ProjectType::Mod,
                Some("1.20.1"),
                Some("optimization"),
                Some("fabric")
            ),
            r#"[["project_type:mod"],["versions:1.20.1"],["categories:optimization"],["categories:fabric"]]"#
        );
        assert_eq!(
            build_facets(ProjectType::Mod, Some("1.20.1"), None, Some("fabric")),
            r#"[["project_type:mod"],["versions:1.20.1"],["categories:fabric"]]"#
        );
        assert_eq!(
            build_facets(ProjectType::ResourcePack, None, None, None),
            r#"[["project_type:resourcepack"]]"#
        );
        assert_eq!(
            build_facets(ProjectType::Shader, None, None, None),
            r#"[["project_type:shader"]]"#
        );
    }

    #[test]
    fn detect_loader_scans_patches_for_known_ids() {
        let mut resolved = Version::new("1.20.1-fabric");
        let mut patch = Version::new("fabric");
        patch.version = Some("0.16.14".to_string());
        resolved.patches = Some(vec![patch]);
        assert_eq!(detect_loader(&resolved), Some("fabric"));

        let vanilla = Version::new("1.20.1");
        assert_eq!(detect_loader(&vanilla), None);
    }

    #[test]
    fn detect_game_version_uses_jar_falling_back_to_id() {
        let mut resolved = Version::new("1.20.1-fabric");
        resolved.jar = Some("1.20.1".to_string());
        assert_eq!(detect_game_version(&resolved), "1.20.1");

        let no_jar = Version::new("1.20.1");
        assert_eq!(detect_game_version(&no_jar), "1.20.1");
    }

    #[test]
    fn project_metadata_exposes_the_online_icon() {
        let project: Project = serde_json::from_str(
            r#"{"id":"AANobbMI","icon_url":"https://cdn.modrinth.com/data/AANobbMI/icon.png"}"#,
        )
        .unwrap();
        assert_eq!(project.id, "AANobbMI");
        assert!(project.icon_url.unwrap().ends_with("/icon.png"));
    }

    #[tokio::test]
    async fn search_mods_parses_real_shaped_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/search"))
            .and(query_param("query", "sodium"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"hits":[{"project_id":"AANobbMI","title":"Sodium","description":"渲染优化","categories":["optimization"],"downloads":123456}],"offset":0,"limit":10,"total_hits":1}"#,
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let provider = DownloadProvider::mojang();
        let api_root = format!("{}/v2", server.uri());

        let params = [
            ("query", "sodium".to_string()),
            ("facets", build_facets(ProjectType::Mod, None, None, None)),
            ("index", "relevance".to_string()),
            ("offset", "0".to_string()),
            ("limit", "10".to_string()),
        ];
        let resp: SearchResponse =
            get_json(&client, &provider, &format!("{api_root}/search"), &params)
                .await
                .unwrap();

        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].project_id, "AANobbMI");
        assert_eq!(resp.hits[0].title, "Sodium");
        assert!(!resp.has_more());
    }

    #[tokio::test]
    async fn install_latest_compatible_downloads_the_primary_file() {
        let server = MockServer::start().await;
        let body = b"fake mod jar bytes".to_vec();
        let sha1 = crate::download::fetch::sha1_hex(&body);
        Mock::given(method("GET"))
            .and(path("/v2/project/sodium/version"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"[{{"id":"version-id","files":[{{"url":"{}/sodium.jar","filename":"sodium.jar","primary":true,"hashes":{{"sha1":"{sha1}"}},"size":{}}}]}}]"#,
                server.uri(),
                body.len()
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/sodium.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let provider = DownloadProvider::mojang();
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test")
            .join("modrinth_install")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = CacheRepository::new(dir.join("cache"));
        let dest_dir = dir.join("mods");

        let versions_url = format!("{}/v2/project/sodium/version", server.uri());
        let versions: Vec<ProjectVersion> = get_json(&client, &provider, &versions_url, &[])
            .await
            .unwrap();
        let dest = install_version_file(&client, &provider, &cache, &versions[0], &dest_dir)
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);

        let custom = install_version_file_as(
            &client,
            &provider,
            &cache,
            &versions[0],
            &dir.join("resourcepacks"),
            "custom-name.zip",
        )
        .await
        .unwrap();
        assert_eq!(custom.file_name().unwrap(), "custom-name.zip");
        assert_eq!(tokio::fs::read(custom).await.unwrap(), body);
    }
}

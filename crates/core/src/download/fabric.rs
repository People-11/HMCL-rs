use serde::Deserialize;

use crate::download::DownloadProvider;
use crate::version::{Arguments, Artifact, Library, Version};

pub const PATCH_ID: &str = "fabric";
const FABRIC_MAVEN: &str = "https://maven.fabricmc.net/";

#[derive(Debug, thiserror::Error)]
pub enum FabricError {
    #[error("no candidate URLs succeeded for {0}")]
    AllCandidatesFailed(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse fabric metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no fabric loader build is available for game version {0}")]
    NoCompatibleBuild(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoaderBuildInfo {
    pub loader: LoaderInfo,
    pub intermediary: IntermediaryInfo,
    #[serde(rename = "launcherMeta")]
    pub launcher_meta: LauncherMeta,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoaderInfo {
    pub maven: String,
    pub version: String,
    #[serde(default)]
    pub stable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntermediaryInfo {
    pub maven: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LauncherMeta {
    #[serde(rename = "mainClass")]
    pub main_class: MainClass,
    pub libraries: LauncherLibraries,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MainClass {
    Simple(String),
    PerSide { client: String },
}

impl MainClass {
    pub fn client(&self) -> &str {
        match self {
            MainClass::Simple(s) => s,
            MainClass::PerSide { client } => client,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LauncherLibraries {
    #[serde(default)]
    pub common: Vec<FabricLibrary>,
    #[serde(default)]
    pub server: Vec<FabricLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FabricLibrary {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

async fn get_text(client: &reqwest::Client, candidates: &[String]) -> Result<String, FabricError> {
    for url in candidates {
        if let Ok(resp) = client.get(url).send().await {
            if let Ok(resp) = resp.error_for_status() {
                if let Ok(text) = resp.text().await {
                    return Ok(text);
                }
            }
        }
    }
    Err(FabricError::AllCandidatesFailed(candidates.join(", ")))
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<Vec<LoaderBuildInfo>, FabricError> {
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{game_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

/// 不指定具体 loader 版本时用: 取该游戏版本的最新构建（不管 stable 与否，跟
/// HMCL UI 默认展示顺序一致，交给调用方自己决定要不要过滤 `stable`）。
pub async fn fetch_latest_build(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<LoaderBuildInfo, FabricError> {
    let mut builds = fetch_compatible_builds(client, provider, game_version).await?;
    if builds.is_empty() {
        return Err(FabricError::NoCompatibleBuild(game_version.to_string()));
    }
    Ok(builds.remove(0))
}

pub async fn fetch_loader_meta(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
    loader_version: &str,
) -> Result<LoaderBuildInfo, FabricError> {
    let url =
        format!("https://meta.fabricmc.net/v2/versions/loader/{game_version}/{loader_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

pub(crate) fn library_from_maven(descriptor: &str, url: Option<String>) -> Option<Library> {
    let artifact = Artifact::from_descriptor(descriptor).ok()?;
    Some(Library {
        artifact,
        url,
        downloads: None,
        extract: None,
        natives: None,
        rules: Vec::new(),
        checksums: None,
        hint: None,
        file_name: None,
    })
}

pub fn build_patch(meta: &LoaderBuildInfo) -> Version {
    let mut libraries = Vec::new();

    for lib in meta
        .launcher_meta
        .libraries
        .common
        .iter()
        .chain(meta.launcher_meta.libraries.server.iter())
    {
        if let Some(library) = library_from_maven(&lib.name, lib.url.clone()) {
            libraries.push(library);
        }
    }

    if let Some(library) =
        library_from_maven(&meta.intermediary.maven, Some(FABRIC_MAVEN.to_string()))
    {
        libraries.push(library);
    }
    if let Some(library) = library_from_maven(&meta.loader.maven, Some(FABRIC_MAVEN.to_string())) {
        libraries.push(library);
    }

    let mut patch = Version::new(PATCH_ID);
    patch.version = Some(meta.loader.version.clone());
    patch.priority = Some(Version::PRIORITY_LOADER);
    patch.main_class = Some(meta.launcher_meta.main_class.client().to_string());
    patch.arguments = Some(Arguments::default());
    patch.libraries = libraries;
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_META: &str = r#"{
        "loader": {"separator": ".", "build": 3, "maven": "net.fabricmc:fabric-loader:0.19.3", "version": "0.19.3", "stable": true},
        "intermediary": {"maven": "net.fabricmc:intermediary:1.20.1", "version": "1.20.1", "stable": true},
        "launcherMeta": {
            "version": 2,
            "mainClass": {"client": "net.fabricmc.loader.impl.launch.knot.KnotClient", "server": "net.fabricmc.loader.impl.launch.knot.KnotServer"},
            "libraries": {
                "client": [{"name": "should-not-be-included:client-only:1.0", "url": "https://maven.fabricmc.net/"}],
                "common": [{"name": "org.ow2.asm:asm:9.10.1", "url": "https://maven.fabricmc.net/"}],
                "server": [{"name": "org.ow2.asm:asm-analysis:9.10.1", "url": "https://maven.fabricmc.net/"}]
            }
        }
    }"#;

    #[test]
    fn parses_real_shaped_fabric_meta_json() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        assert_eq!(meta.loader.version, "0.19.3");
        assert_eq!(
            meta.launcher_meta.main_class.client(),
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
    }

    #[test]
    fn build_patch_includes_common_and_server_libs_but_not_client() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        let patch = build_patch(&meta);

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(patch.version.as_deref(), Some("0.19.3"));
        assert_eq!(patch.priority, Some(Version::PRIORITY_LOADER));
        assert_eq!(
            patch.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );

        assert!(
            patch.libraries.iter().any(|l| l.is("org.ow2.asm", "asm")),
            "common library must be included"
        );
        assert!(
            patch
                .libraries
                .iter()
                .any(|l| l.is("org.ow2.asm", "asm-analysis")),
            "server library must be included"
        );
        assert!(
            !patch
                .libraries
                .iter()
                .any(|l| l.is("should-not-be-included", "client-only")),
            "client-only libraries are deliberately excluded, matching HMCL"
        );
        assert!(
            patch
                .libraries
                .iter()
                .any(|l| l.is("net.fabricmc", "intermediary")),
            "intermediary mapping jar must be added explicitly"
        );
        assert!(
            patch
                .libraries
                .iter()
                .any(|l| l.is("net.fabricmc", "fabric-loader")),
            "the loader jar itself must be added explicitly"
        );
    }

    #[test]
    fn simple_string_main_class_is_also_supported() {
        let meta: LoaderBuildInfo = serde_json::from_str(
            r#"{
                "loader": {"separator": ".", "build": 1, "maven": "net.fabricmc:fabric-loader:0.1.0", "version": "0.1.0", "stable": false},
                "intermediary": {"maven": "net.fabricmc:intermediary:1.14", "version": "1.14"},
                "launcherMeta": {"version": 1, "mainClass": "net.fabricmc.loader.launch.knot.KnotClient", "libraries": {"common": [], "server": []}}
            }"#,
        )
        .unwrap();
        assert_eq!(
            meta.launcher_meta.main_class.client(),
            "net.fabricmc.loader.launch.knot.KnotClient"
        );
    }

    #[test]
    fn patch_resolves_onto_a_vanilla_version_and_overrides_main_class() {
        use std::collections::HashMap;

        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        let patch = build_patch(&meta);

        let mut vanilla = Version::new("1.20.1");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        vanilla.libraries = vec![Library {
            artifact: Artifact::new("com.mojang", "vanilla-lib", "1.0"),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }];

        let mut instance = Version::new("1.20.1-fabric");
        instance.inherits_from = Some("1.20.1".to_string());
        instance.patches = Some(vec![patch]);

        let mut provider = HashMap::new();
        provider.insert("1.20.1".to_string(), vanilla);

        let resolved = instance.resolve(&provider).unwrap();
        assert_eq!(
            resolved.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("com.mojang", "vanilla-lib")));
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("net.fabricmc", "fabric-loader")));
    }
}

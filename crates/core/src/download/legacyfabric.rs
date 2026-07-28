use serde::Deserialize;

use super::fabric::{library_from_maven, LauncherLibraries, MainClass};
use super::DownloadProvider;
use crate::version::{Arguments, Artifact, Version};

pub const PATCH_ID: &str = "legacyfabric";

#[derive(Debug, thiserror::Error)]
pub enum LegacyFabricError {
    #[error("no candidate URLs succeeded for {0}")]
    AllCandidatesFailed(String),
    #[error("failed to parse legacyfabric metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no legacyfabric loader build is available for game version {0}")]
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

pub fn normalize_game_version(version: &str) -> String {
    match version.strip_prefix("2point0_") {
        Some(rest) => format!("2.0_{rest}"),
        None => version.to_string(),
    }
}

async fn get_text(
    client: &reqwest::Client,
    candidates: &[String],
) -> Result<String, LegacyFabricError> {
    for url in candidates {
        if let Ok(resp) = client.get(url).send().await {
            if let Ok(resp) = resp.error_for_status() {
                if let Ok(text) = resp.text().await {
                    return Ok(text);
                }
            }
        }
    }
    Err(LegacyFabricError::AllCandidatesFailed(
        candidates.join(", "),
    ))
}

/// 注意: `game_version` 要传 LegacyFabric Meta API 认得的原始形式（即
/// [`normalize_game_version`] 转换之前的那个），因为这个函数直接拼进请求 URL——
/// 跟 Java 版行为一致，`normalizeVersion` 只用来生成给用户看/给 `Version.id` 用的
/// 显示名，从不用归一化后的字符串去请求 API。
pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<Vec<LoaderBuildInfo>, LegacyFabricError> {
    let url = format!("https://meta.legacyfabric.net/v2/versions/loader/{game_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<LoaderBuildInfo, LegacyFabricError> {
    let mut builds = fetch_compatible_builds(client, provider, game_version).await?;
    if builds.is_empty() {
        return Err(LegacyFabricError::NoCompatibleBuild(
            game_version.to_string(),
        ));
    }
    Ok(builds.remove(0))
}

pub async fn fetch_loader_meta(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
    loader_version: &str,
) -> Result<LoaderBuildInfo, LegacyFabricError> {
    let url =
        format!("https://meta.legacyfabric.net/v2/versions/loader/{game_version}/{loader_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

fn maven_repo_for_group(group: &str) -> &'static str {
    match group {
        "net.legacyfabric" => "https://maven.legacyfabric.net/",
        _ => "https://maven.fabricmc.net/",
    }
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

    if let Ok(artifact) = Artifact::from_descriptor(&meta.intermediary.maven) {
        let repo = maven_repo_for_group(&artifact.group).to_string();
        if let Some(library) = library_from_maven(&meta.intermediary.maven, Some(repo)) {
            libraries.push(library);
        }
    }
    if let Ok(artifact) = Artifact::from_descriptor(&meta.loader.maven) {
        let repo = maven_repo_for_group(&artifact.group).to_string();
        if let Some(library) = library_from_maven(&meta.loader.maven, Some(repo)) {
            libraries.push(library);
        }
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
        "intermediary": {"maven": "net.legacyfabric:intermediary:1.12.2", "version": "1.12.2", "stable": true},
        "launcherMeta": {
            "version": 2,
            "mainClass": {"client": "net.fabricmc.loader.impl.launch.knot.KnotClient"},
            "libraries": {
                "client": [{"name": "should-not-be-included:client-only:1.0", "url": "https://maven.fabricmc.net/"}],
                "common": [{"name": "org.ow2.asm:asm:9.10.1", "url": "https://maven.fabricmc.net/"}],
                "server": []
            }
        }
    }"#;

    #[test]
    fn normalizes_april_fools_2013_version_codenames() {
        assert_eq!(normalize_game_version("2point0_purple"), "2.0_purple");
        assert_eq!(normalize_game_version("2point0_blue"), "2.0_blue");
        assert_eq!(
            normalize_game_version("1.12.2"),
            "1.12.2",
            "normal version strings must pass through unchanged"
        );
    }

    #[test]
    fn parses_real_shaped_legacyfabric_meta_json() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        assert_eq!(meta.loader.version, "0.19.3");
        assert_eq!(
            meta.intermediary.maven,
            "net.legacyfabric:intermediary:1.12.2"
        );
    }

    #[test]
    fn build_patch_routes_legacyfabric_group_to_its_own_maven() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        let patch = build_patch(&meta);

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(
            patch.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert!(!patch
            .libraries
            .iter()
            .any(|l| l.is("should-not-be-included", "client-only")));

        let intermediary_lib = patch
            .libraries
            .iter()
            .find(|l| l.is("net.legacyfabric", "intermediary"))
            .expect("intermediary must be present");
        assert_eq!(
            intermediary_lib.url.as_deref(),
            Some("https://maven.legacyfabric.net/"),
            "net.legacyfabric-group artifacts must route to legacyfabric's own maven"
        );

        let loader_lib = patch
            .libraries
            .iter()
            .find(|l| l.is("net.fabricmc", "fabric-loader"))
            .expect("loader jar must be present");
        assert_eq!(
            loader_lib.url.as_deref(),
            Some("https://maven.fabricmc.net/"),
            "net.fabricmc-group artifacts must route to fabric's maven even under legacyfabric"
        );
    }

    #[test]
    fn patch_resolves_onto_a_vanilla_version_and_overrides_main_class() {
        use crate::version::Library;
        use std::collections::HashMap;

        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        let patch = build_patch(&meta);

        let mut vanilla = Version::new("1.12.2");
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

        let mut instance = Version::new("1.12.2-legacyfabric");
        instance.inherits_from = Some("1.12.2".to_string());
        instance.patches = Some(vec![patch]);

        let mut provider = HashMap::new();
        provider.insert("1.12.2".to_string(), vanilla);

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
            .any(|l| l.is("net.legacyfabric", "intermediary")));
    }
}

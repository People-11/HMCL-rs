use serde::Deserialize;

use super::fabric::{library_from_maven, LauncherLibraries, MainClass};
use super::DownloadProvider;
use crate::version::{Arguments, Artifact, Version};

pub const PATCH_ID: &str = "quilt";

#[derive(Debug, thiserror::Error)]
pub enum QuiltError {
    #[error("no candidate URLs succeeded for {0}")]
    AllCandidatesFailed(String),
    #[error("failed to parse quilt metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no quilt loader build is available for game version {0}")]
    NoCompatibleBuild(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoaderBuildInfo {
    pub loader: LoaderInfo,
    #[serde(default)]
    pub intermediary: Option<IntermediaryInfo>,
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

async fn get_text(client: &reqwest::Client, candidates: &[String]) -> Result<String, QuiltError> {
    for url in candidates {
        if let Ok(resp) = client.get(url).send().await {
            if let Ok(resp) = resp.error_for_status() {
                if let Ok(text) = resp.text().await {
                    return Ok(text);
                }
            }
        }
    }
    Err(QuiltError::AllCandidatesFailed(candidates.join(", ")))
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<Vec<LoaderBuildInfo>, QuiltError> {
    let url = format!("https://meta.quiltmc.org/v3/versions/loader/{game_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<LoaderBuildInfo, QuiltError> {
    let mut builds = fetch_compatible_builds(client, provider, game_version).await?;
    if builds.is_empty() {
        return Err(QuiltError::NoCompatibleBuild(game_version.to_string()));
    }
    Ok(builds.remove(0))
}

pub async fn fetch_loader_meta(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
    loader_version: &str,
) -> Result<LoaderBuildInfo, QuiltError> {
    let url =
        format!("https://meta.quiltmc.org/v3/versions/loader/{game_version}/{loader_version}");
    let text = get_text(client, &provider.inject_url_candidates(&url)).await?;
    Ok(serde_json::from_str(&text)?)
}

fn maven_repo_for_group(group: &str) -> &'static str {
    match group {
        "org.quiltmc" => "https://maven.quiltmc.org/repository/release/",
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

    if let Some(intermediary) = &meta.intermediary {
        if let Ok(artifact) = Artifact::from_descriptor(&intermediary.maven) {
            let repo = maven_repo_for_group(&artifact.group).to_string();
            if let Some(library) = library_from_maven(&intermediary.maven, Some(repo)) {
                libraries.push(library);
            }
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
        "loader": {"maven": "org.quiltmc:quilt-loader:0.20.0-beta.9", "version": "0.20.0-beta.9", "build": 9, "separator": "."},
        "hashed": {"maven": "org.quiltmc:hashed:1.20.1", "version": "1.20.1"},
        "intermediary": {"maven": "net.fabricmc:intermediary:1.20.1", "version": "1.20.1"},
        "launcherMeta": {
            "version": 1,
            "mainClass": {"client": "org.quiltmc.loader.impl.launch.knot.KnotClient", "server": "org.quiltmc.loader.impl.launch.knot.KnotServer", "serverLauncher": "org.quiltmc.loader.impl.launch.server.QuiltServerLauncher"},
            "libraries": {
                "client": [{"name": "should-not-be-included:client-only:1.0", "url": "https://maven.fabricmc.net/"}],
                "common": [{"name": "net.fabricmc:tiny-mappings-parser:0.3.0+build.17", "url": "https://maven.fabricmc.net/"}],
                "server": [{"name": "org.quiltmc:quilt-json5:1.0.4", "url": "https://maven.quiltmc.org/repository/release/"}]
            }
        }
    }"#;

    #[test]
    fn parses_real_shaped_quilt_meta_json_with_a_three_field_main_class() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        assert_eq!(meta.loader.version, "0.20.0-beta.9");
        assert_eq!(
            meta.launcher_meta.main_class.client(),
            "org.quiltmc.loader.impl.launch.knot.KnotClient"
        );
    }

    #[test]
    fn build_patch_routes_libraries_to_the_correct_maven_by_group() {
        let meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        let patch = build_patch(&meta);

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(patch.version.as_deref(), Some("0.20.0-beta.9"));
        assert_eq!(
            patch.main_class.as_deref(),
            Some("org.quiltmc.loader.impl.launch.knot.KnotClient")
        );

        assert!(
            !patch
                .libraries
                .iter()
                .any(|l| l.is("should-not-be-included", "client-only")),
            "client-only libraries must be excluded, same as fabric"
        );

        let loader_lib = patch
            .libraries
            .iter()
            .find(|l| l.is("org.quiltmc", "quilt-loader"))
            .expect("loader jar must be present");
        assert_eq!(
            loader_lib.url.as_deref(),
            Some("https://maven.quiltmc.org/repository/release/"),
            "quilt-loader itself must route to quilt's own maven, not fabric's"
        );

        let intermediary_lib = patch
            .libraries
            .iter()
            .find(|l| l.is("net.fabricmc", "intermediary"))
            .expect("intermediary must be present when the response includes it");
        assert_eq!(
            intermediary_lib.url.as_deref(),
            Some("https://maven.fabricmc.net/"),
            "net.fabricmc-group libraries must route to fabric's maven even under quilt"
        );
    }

    #[test]
    fn build_patch_tolerates_a_response_without_intermediary() {
        let mut meta: LoaderBuildInfo = serde_json::from_str(SAMPLE_META).unwrap();
        meta.intermediary = None;
        let patch = build_patch(&meta);
        assert!(!patch
            .libraries
            .iter()
            .any(|l| l.is("net.fabricmc", "intermediary")));
        assert!(
            patch
                .libraries
                .iter()
                .any(|l| l.is("org.quiltmc", "quilt-loader")),
            "loader jar must still be added"
        );
    }

    #[test]
    fn patch_resolves_onto_a_vanilla_version_and_overrides_main_class() {
        use crate::version::Library;
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

        let mut instance = Version::new("1.20.1-quilt");
        instance.inherits_from = Some("1.20.1".to_string());
        instance.patches = Some(vec![patch]);

        let mut provider = HashMap::new();
        provider.insert("1.20.1".to_string(), vanilla);

        let resolved = instance.resolve(&provider).unwrap();
        assert_eq!(
            resolved.main_class.as_deref(),
            Some("org.quiltmc.loader.impl.launch.knot.KnotClient")
        );
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("com.mojang", "vanilla-lib")));
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("org.quiltmc", "quilt-loader")));
    }
}

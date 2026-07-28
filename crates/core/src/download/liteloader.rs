use std::collections::HashMap;

use serde::Deserialize;

use super::DownloadProvider;
use crate::version::{
    Argument, Arguments, Artifact, DownloadInfo, LibrariesDownloadInfo, Library,
    LibraryDownloadInfo, Version,
};

pub const PATCH_ID: &str = "liteloader";
pub const PRIORITY_LITELOADER: i32 = 60000;
const TWEAK_CLASS: &str = "com.mumfrey.liteloader.launch.LiteLoaderTweaker";
const LAUNCH_WRAPPER_MAIN: &str = "net.minecraft.launchwrapper.Launch";
const VERSIONS_LIST_URL: &str = "https://dl.liteloader.com/versions/versions.json";

#[derive(Debug, thiserror::Error)]
pub enum LiteLoaderError {
    #[error("no candidate URLs succeeded for {0}")]
    AllCandidatesFailed(String),
    #[error("failed to parse liteloader metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no liteloader build is available for game version {0}")]
    NoCompatibleBuild(String),
    #[error("no liteloader build with version {0:?} is available for game version {1}")]
    VersionNotFound(String, String),
}

#[derive(Debug, Deserialize)]
struct VersionsRoot {
    versions: HashMap<String, GameVersions>,
}

#[derive(Debug, Deserialize)]
struct GameVersions {
    repo: Option<Repository>,
    artefacts: Option<Branch>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    url: String,
}

#[derive(Debug, Deserialize)]
struct Branch {
    #[serde(rename = "com.mumfrey:liteloader")]
    lite_loader: HashMap<String, LiteLoaderBuild>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiteLoaderBuild {
    pub file: String,
    pub version: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub libraries: Vec<Library>,
}

async fn get_text(
    client: &reqwest::Client,
    candidates: &[String],
) -> Result<String, LiteLoaderError> {
    for url in candidates {
        if let Ok(resp) = client.get(url).send().await {
            if let Ok(resp) = resp.error_for_status() {
                if let Ok(text) = resp.text().await {
                    return Ok(text);
                }
            }
        }
    }
    Err(LiteLoaderError::AllCandidatesFailed(candidates.join(", ")))
}

async fn fetch_root(
    client: &reqwest::Client,
    provider: &DownloadProvider,
) -> Result<VersionsRoot, LiteLoaderError> {
    let text = get_text(client, &provider.inject_url_candidates(VERSIONS_LIST_URL)).await?;
    Ok(serde_json::from_str(&text)?)
}

pub async fn fetch_compatible_builds(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<(String, Vec<LiteLoaderBuild>), LiteLoaderError> {
    let root = fetch_root(client, provider).await?;
    let game = root
        .versions
        .get(game_version)
        .ok_or_else(|| LiteLoaderError::NoCompatibleBuild(game_version.to_string()))?;
    let branch = game
        .artefacts
        .as_ref()
        .ok_or_else(|| LiteLoaderError::NoCompatibleBuild(game_version.to_string()))?;
    let repo_url = game
        .repo
        .as_ref()
        .map(|r| r.url.clone())
        .unwrap_or_default();
    let builds: Vec<LiteLoaderBuild> = branch
        .lite_loader
        .iter()
        .filter(|(key, _)| key.as_str() != "latest")
        .map(|(_, build)| build.clone())
        .collect();
    Ok((repo_url, builds))
}

pub async fn fetch_latest_build(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
) -> Result<(String, LiteLoaderBuild), LiteLoaderError> {
    let (repo_url, mut builds) = fetch_compatible_builds(client, provider, game_version).await?;
    if builds.is_empty() {
        return Err(LiteLoaderError::NoCompatibleBuild(game_version.to_string()));
    }
    builds.sort_by_key(|b| b.timestamp.parse::<u64>().unwrap_or(0));
    Ok((repo_url, builds.pop().expect("checked non-empty above")))
}

pub async fn fetch_build_by_version(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
    version: &str,
) -> Result<(String, LiteLoaderBuild), LiteLoaderError> {
    let (repo_url, builds) = fetch_compatible_builds(client, provider, game_version).await?;
    builds
        .into_iter()
        .find(|b| b.version == version)
        .map(|b| (repo_url, b))
        .ok_or_else(|| {
            LiteLoaderError::VersionNotFound(version.to_string(), game_version.to_string())
        })
}

pub fn build_patch(game_version: &str, repo_url: &str, build: &LiteLoaderBuild) -> Version {
    let artifact_url = format!(
        "{repo_url}com/mumfrey/liteloader/{game_version}/{}",
        build.file
    );
    let liteloader_library = Library {
        artifact: Artifact::new("com.mumfrey", "liteloader", &build.version),
        url: Some("http://dl.liteloader.com/versions/".to_string()),
        downloads: Some(LibrariesDownloadInfo {
            artifact: Some(LibraryDownloadInfo {
                path: None,
                download: DownloadInfo {
                    url: Some(artifact_url),
                    sha1: None,
                    size: 0,
                },
            }),
            classifiers: HashMap::new(),
        }),
        extract: None,
        natives: None,
        rules: Vec::new(),
        checksums: None,
        hint: None,
        file_name: None,
    };

    let mut libraries = build.libraries.clone();
    libraries.push(liteloader_library);

    let mut patch = Version::new(PATCH_ID);
    patch.version = Some(build.version.clone());
    patch.priority = Some(PRIORITY_LITELOADER);
    patch.main_class = Some(LAUNCH_WRAPPER_MAIN.to_string());
    patch.arguments = Some(Arguments {
        game: Some(vec![
            Argument::Plain("--tweakClass".to_string()),
            Argument::Plain(TWEAK_CLASS.to_string()),
        ]),
        jvm: None,
    });
    patch.logging = Some(HashMap::new());
    patch.libraries = libraries;
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ROOT: &str = r#"{
        "versions": {
            "1.7.10": {
                "repo": {"stream": "RELEASE", "type": "ivy", "url": "http://dl.liteloader.com/versions/", "classifier": "mcpnames"},
                "artefacts": {
                    "com.mumfrey:liteloader": {
                        "latest": {"file": "liteloader-1.7.10_04.jar", "version": "1.7.10_04", "timestamp": "1414368553", "libraries": []},
                        "63ada46e033d0cb6782bada09ad5ca4e": {
                            "tweakClass": "com.mumfrey.liteloader.launch.LiteLoaderTweaker",
                            "libraries": [{"name": "net.minecraft:launchwrapper:1.11"}, {"name": "org.ow2.asm:asm-all:5.0.3"}],
                            "file": "liteloader-1.7.10_04.jar", "version": "1.7.10_04", "md5": "63ada46e033d0cb6782bada09ad5ca4e", "timestamp": "1414368553"
                        },
                        "db7235aefd407ac1fde09a7baba50839": {
                            "tweakClass": "com.mumfrey.liteloader.launch.LiteLoaderTweaker",
                            "libraries": [{"name": "net.minecraft:launchwrapper:1.9"}, {"name": "org.ow2.asm:asm-all:5.0.3", "url": "http://repo.maven.apache.org/maven2/"}],
                            "file": "liteloader-1.7.10_00.jar", "version": "1.7.10_00", "md5": "db7235aefd407ac1fde09a7baba50839", "timestamp": "1404330030"
                        }
                    }
                }
            }
        }
    }"#;

    #[test]
    fn parses_real_shaped_versions_json_and_skips_the_latest_alias() {
        let root: VersionsRoot = serde_json::from_str(SAMPLE_ROOT).unwrap();
        let game = root.versions.get("1.7.10").unwrap();
        let branch = game.artefacts.as_ref().unwrap();
        assert_eq!(
            branch.lite_loader.len(),
            3,
            "raw parse keeps \"latest\" too, filtering happens in fetch_compatible_builds"
        );
        assert_eq!(
            game.repo.as_ref().unwrap().url,
            "http://dl.liteloader.com/versions/"
        );
    }

    fn sample_root() -> VersionsRoot {
        serde_json::from_str(SAMPLE_ROOT).unwrap()
    }

    fn extract_builds(root: &VersionsRoot, game_version: &str) -> (String, Vec<LiteLoaderBuild>) {
        let game = root.versions.get(game_version).unwrap();
        let branch = game.artefacts.as_ref().unwrap();
        let repo_url = game.repo.as_ref().unwrap().url.clone();
        (
            repo_url,
            branch
                .lite_loader
                .iter()
                .filter(|(k, _)| k.as_str() != "latest")
                .map(|(_, b)| b.clone())
                .collect(),
        )
    }

    #[test]
    fn latest_alias_key_is_excluded_from_the_real_build_list() {
        let root = sample_root();
        let (_, builds) = extract_builds(&root, "1.7.10");
        assert_eq!(builds.len(), 2);
        assert!(builds.iter().any(|b| b.version == "1.7.10_04"));
        assert!(builds.iter().any(|b| b.version == "1.7.10_00"));
    }

    #[test]
    fn build_patch_matches_java_semantics() {
        let root = sample_root();
        let (repo_url, builds) = extract_builds(&root, "1.7.10");
        let newest = builds
            .iter()
            .max_by_key(|b| b.timestamp.parse::<u64>().unwrap_or(0))
            .unwrap();
        assert_eq!(newest.version, "1.7.10_04");

        let patch = build_patch("1.7.10", &repo_url, newest);

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(patch.version.as_deref(), Some("1.7.10_04"));
        assert_eq!(patch.priority, Some(PRIORITY_LITELOADER));
        assert_eq!(patch.main_class.as_deref(), Some(LAUNCH_WRAPPER_MAIN));
        assert_eq!(
            patch.logging,
            Some(HashMap::new()),
            "logging must be explicitly cleared, not just absent"
        );

        let game_args = patch.arguments.as_ref().unwrap().game.as_ref().unwrap();
        assert!(matches!(&game_args[0], Argument::Plain(s) if s == "--tweakClass"));
        assert!(matches!(&game_args[1], Argument::Plain(s) if s == TWEAK_CLASS));

        assert!(
            patch
                .libraries
                .iter()
                .any(|l| l.is("net.minecraft", "launchwrapper")),
            "must carry the loader-declared dependency libraries"
        );
        assert!(patch
            .libraries
            .iter()
            .any(|l| l.is("org.ow2.asm", "asm-all")));
        let ll = patch
            .libraries
            .iter()
            .find(|l| l.is("com.mumfrey", "liteloader"))
            .expect("liteloader itself must be a library");
        assert_eq!(
            ll.downloads.as_ref().unwrap().artifact.as_ref().unwrap().download.url.as_deref(),
            Some("http://dl.liteloader.com/versions/com/mumfrey/liteloader/1.7.10/liteloader-1.7.10_04.jar")
        );
    }

    #[test]
    fn patch_resolves_onto_a_vanilla_version_and_wins_the_main_class_over_a_lower_priority_patch() {
        use crate::version::{CompatibilityRule, RuleAction};
        use std::collections::HashMap as Map;

        let root = sample_root();
        let (repo_url, builds) = extract_builds(&root, "1.7.10");
        let build = builds.iter().find(|b| b.version == "1.7.10_04").unwrap();
        let liteloader_patch = build_patch("1.7.10", &repo_url, build);

        let mut vanilla = Version::new("1.7.10");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        vanilla.logging = Some(Map::from([(
            crate::version::DownloadType::Client,
            crate::version::LoggingInfo {
                file: crate::version::IdDownloadInfo {
                    id: "client-1.7.xml".to_string(),
                    download: DownloadInfo::new("https://example.com/client-1.7.xml"),
                },
                argument: "-Dlog4j.configurationFile=${path}".to_string(),
                log_type: "log4j2-xml".to_string(),
            },
        )]));

        let mut lower_priority_patch = Version::new("some-lower-priority-thing");
        lower_priority_patch.priority = Some(30000);
        lower_priority_patch.main_class = Some("should.lose.To.LiteLoader".to_string());
        lower_priority_patch.compatibility_rules = vec![CompatibilityRule {
            action: RuleAction::Allow,
            os: None,
            features: None,
        }];

        let mut instance = Version::new("1.7.10-liteloader");
        instance.inherits_from = Some("1.7.10".to_string());
        instance.patches = Some(vec![liteloader_patch, lower_priority_patch]);

        let mut provider = Map::new();
        provider.insert("1.7.10".to_string(), vanilla);

        let resolved = instance.resolve(&provider).expect("chain resolves");
        assert_eq!(
            resolved.main_class.as_deref(),
            Some(LAUNCH_WRAPPER_MAIN),
            "liteloader (priority 60000) must win over the lower-priority patch (30000)"
        );
        assert_eq!(resolved.logging, Some(Map::new()), "liteloader's explicit empty logging must suppress vanilla's, even through a multi-patch merge");
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("com.mumfrey", "liteloader")));
    }
}

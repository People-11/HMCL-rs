use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::download::{
    fabric, forge, forge_old, neoforge, optifine, quilt, CacheRepository, DownloadProvider,
    FetchError, ProgressSink, DEFAULT_BMCLAPI_API_ROOT,
};
use crate::install::{self, GameRepository, InstallReport};
use crate::java::{find_a_java, JavaDetectError};
use crate::version::{Env, Version, VersionError};
use crate::versioning::GameVersionNumber;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderKind {
    Forge,
    NeoForge,
    OptiFine,
    Fabric,
    Quilt,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 5] = [
        LoaderKind::Forge,
        LoaderKind::NeoForge,
        LoaderKind::OptiFine,
        LoaderKind::Fabric,
        LoaderKind::Quilt,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
            LoaderKind::OptiFine => "optifine",
            LoaderKind::Fabric => "fabric",
            LoaderKind::Quilt => "quilt",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
            LoaderKind::OptiFine => "OptiFine",
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Quilt => "Quilt",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.slug() == slug)
    }
}

#[derive(Debug, Clone)]
pub struct LoaderSelection {
    pub kind: LoaderKind,
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GameInstallError {
    #[error(transparent)]
    Install(#[from] install::InstallError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Fabric(#[from] fabric::FabricError),
    #[error(transparent)]
    Quilt(#[from] quilt::QuiltError),
    #[error(transparent)]
    Forge(#[from] forge::ForgeInstallError),
    #[error(transparent)]
    ForgeOld(#[from] forge_old::ForgeOldInstallError),
    #[error(transparent)]
    NeoForge(#[from] neoforge::NeoForgeInstallError),
    #[error(transparent)]
    OptiFine(#[from] optifine::OptiFineInstallError),
    #[error(transparent)]
    Java(#[from] JavaDetectError),
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("version {0} was not found in the Mojang manifest")]
    MissingGameVersion(String),
    #[error("instance {0} already exists")]
    InstanceAlreadyExists(String),
    #[error("invalid instance name {0:?}; use only ASCII letters, digits, '.', '-' and '_'")]
    InvalidInstanceName(String),
    #[error("a loader instance must use a different name from its parent game version")]
    LoaderInstanceMustDiffer,
    #[error("instance {0} was not found")]
    MissingInstance(String),
    #[error("{0} is not installed in this instance")]
    LoaderNotInstalled(String),
}

pub async fn fetch_loader_versions(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    game_version: &str,
    kind: LoaderKind,
) -> Result<Vec<String>, GameInstallError> {
    let mut versions: Vec<String> = match kind {
        LoaderKind::Fabric => fabric::fetch_compatible_builds(client, provider, game_version)
            .await?
            .into_iter()
            .map(|build| build.loader.version)
            .collect(),
        LoaderKind::Quilt => quilt::fetch_compatible_builds(client, provider, game_version)
            .await?
            .into_iter()
            .map(|build| build.loader.version)
            .collect(),
        LoaderKind::Forge => {
            forge::fetch_compatible_builds(client, DEFAULT_BMCLAPI_API_ROOT, game_version)
                .await?
                .into_iter()
                .map(|build| build.version)
                .collect()
        }
        LoaderKind::NeoForge => {
            neoforge::fetch_compatible_builds(client, DEFAULT_BMCLAPI_API_ROOT, game_version)
                .await?
                .into_iter()
                .map(|build| build.version)
                .collect()
        }
        LoaderKind::OptiFine => {
            optifine::fetch_compatible_builds(client, DEFAULT_BMCLAPI_API_ROOT, game_version)
                .await?
                .into_iter()
                .map(|build| build.version)
                .collect()
        }
    };
    if matches!(
        kind,
        LoaderKind::Forge | LoaderKind::NeoForge | LoaderKind::OptiFine
    ) {
        versions.reverse();
    }
    Ok(versions)
}

fn installer_cache_path(game_dir: &Path, loader: LoaderKind, version: &str) -> PathBuf {
    game_dir
        .join(".hmcl-rs-cache")
        .join("installers")
        .join(format!("{}-{version}-installer.jar", loader.slug()))
}

fn instance_recipe(game_version: &str, instance_id: &str, patch: Version) -> Version {
    let mut instance = Version::new(instance_id);
    instance.inherits_from = Some(game_version.to_string());
    instance.patches = Some(vec![patch]);
    instance
}

fn is_loader_patch(patch: &Version) -> bool {
    LoaderKind::from_slug(&patch.id).is_some()
}

pub fn game_version_of(instance: &Version, resolved: &Version) -> String {
    instance
        .inherits_from
        .clone()
        .or_else(|| {
            instance
                .patches
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|patch| patch.id == "game")
                .and_then(|patch| patch.version.clone())
        })
        .or_else(|| resolved.jar.clone())
        .unwrap_or_else(|| instance.id.clone())
}

fn vanilla_for_installer(
    versions: &HashMap<String, Version>,
    instance: &Version,
    game_version: &str,
) -> Version {
    if let Some(vanilla) = versions.get(game_version) {
        return vanilla.clone();
    }
    if let Some(game_patch) = instance
        .patches
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|patch| patch.id == "game")
    {
        let mut vanilla = game_patch.clone();
        vanilla.id = game_version.to_string();
        vanilla.version = None;
        vanilla.priority = None;
        vanilla.hidden = Some(false);
        vanilla.root = Some(true);
        return vanilla;
    }
    instance.clone()
}

fn with_loader_patch(instance: &Version, game_version: &str, patch: Version) -> Version {
    if instance.inherits_from.is_some() || instance.patches.is_some() {
        let mut updated = instance.clone();
        let patches = updated.patches.get_or_insert_default();
        patches.retain(|existing| !is_loader_patch(existing));
        patches.push(patch);
        return updated;
    }

    // 原版 JSON 本身不能同时充当 HMCL 的 root recipe：root 有 patches 时，
    // resolve 只合并 patches。把完整原版内容收进 game patch，实例 id 保持不变。
    let mut game_patch = instance.clone();
    game_patch.id = "game".to_string();
    game_patch.version = Some(game_version.to_string());
    game_patch.priority = Some(Version::PRIORITY_MC);
    game_patch.inherits_from = None;
    game_patch.hidden = Some(true);
    game_patch.root = Some(false);
    game_patch.patches = None;

    let mut updated = Version::new(&instance.id);
    updated.jar = Some(game_version.to_string());
    updated.patches = Some(vec![game_patch, patch]);
    updated
}

async fn build_loader_patch(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    game_version: &str,
    vanilla: &Version,
    selection: &LoaderSelection,
) -> Result<Version, GameInstallError> {
    Ok(match selection.kind {
        LoaderKind::Fabric => {
            let meta =
                fabric::fetch_loader_meta(client, provider, game_version, &selection.version)
                    .await?;
            fabric::build_patch(&meta)
        }
        LoaderKind::Quilt => {
            let meta = quilt::fetch_loader_meta(client, provider, game_version, &selection.version)
                .await?;
            quilt::build_patch(&meta)
        }
        LoaderKind::Forge => {
            let build = forge::fetch_build_by_version(
                client,
                DEFAULT_BMCLAPI_API_ROOT,
                game_version,
                &selection.version,
            )
            .await?;
            let installer = installer_cache_path(game_dir, selection.kind, &selection.version);
            forge::download_installer(client, provider, &build, &installer).await?;

            if GameVersionNumber::compare(game_version, "1.13").is_some_and(|order| order.is_lt()) {
                forge_old::install_old_forge(
                    client,
                    provider,
                    cache,
                    repo,
                    &installer,
                    &selection.version,
                )
                .await?
            } else {
                install::install_client_jar(client, provider, cache, repo, vanilla).await?;
                let java = find_a_java(None)?;
                forge::install_new_forge(
                    client,
                    provider,
                    cache,
                    repo,
                    &installer,
                    vanilla,
                    &java.binary,
                    forge::PATCH_ID,
                    &selection.version,
                )
                .await?
            }
        }
        LoaderKind::NeoForge => {
            let build = neoforge::fetch_build_by_version(
                client,
                DEFAULT_BMCLAPI_API_ROOT,
                game_version,
                &selection.version,
            )
            .await?;
            let installer = installer_cache_path(game_dir, selection.kind, &selection.version);
            neoforge::download_installer(client, provider, &build, &installer).await?;
            install::install_client_jar(client, provider, cache, repo, vanilla).await?;
            let java = find_a_java(None)?;
            neoforge::install_neoforge(
                client,
                provider,
                cache,
                repo,
                &installer,
                vanilla,
                &java.binary,
            )
            .await?
        }
        LoaderKind::OptiFine => {
            let build = optifine::fetch_build_by_version(
                client,
                DEFAULT_BMCLAPI_API_ROOT,
                game_version,
                &selection.version,
            )
            .await?;
            let installer = installer_cache_path(game_dir, selection.kind, &selection.version);
            optifine::download_installer(client, &build, &installer).await?;
            install::install_client_jar(client, provider, cache, repo, vanilla).await?;
            let base = vanilla.resolve(&HashMap::new())?;
            let java = find_a_java(None)?;
            optifine::install_optifine(
                repo,
                &installer,
                &base.id,
                base.main_class.as_deref().unwrap_or(""),
                &java.binary,
            )
            .await?
        }
    })
}

pub fn is_valid_instance_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

#[allow(clippy::too_many_arguments)]
pub async fn install_game_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    game_version: &str,
    instance_id: &str,
    loader: Option<&LoaderSelection>,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, GameInstallError> {
    if !is_valid_instance_name(instance_id) {
        return Err(GameInstallError::InvalidInstanceName(
            instance_id.to_string(),
        ));
    }
    if loader.is_some() && instance_id == game_version {
        return Err(GameInstallError::LoaderInstanceMustDiffer);
    }
    if instance_id != game_version && repo.version_json_path(instance_id).is_file() {
        return Err(GameInstallError::InstanceAlreadyExists(
            instance_id.to_string(),
        ));
    }

    let parent_was_visible = repo
        .load_all_versions()
        .get(game_version)
        .is_some_and(|version| !version.is_hidden());
    let manifest = install::fetch_version_manifest(client, provider).await?;
    let entry = manifest
        .find(game_version)
        .ok_or_else(|| GameInstallError::MissingGameVersion(game_version.to_string()))?;
    let mut raw =
        install::download_version_json(client, provider, repo, game_version, entry).await?;

    if let (Some(selection), Some(progress)) = (loader, progress) {
        let _ = progress.send(crate::download::ProgressEvent::LoaderStarted {
            name: selection.kind.display_name().to_string(),
        });
    }

    let patch = match loader {
        Some(selection) => Some(
            build_loader_patch(
                client,
                provider,
                cache,
                repo,
                game_dir,
                game_version,
                &raw,
                selection,
            )
            .await?,
        ),
        None => None,
    };

    if loader.is_some() {
        if let Some(progress) = progress {
            let _ = progress.send(crate::download::ProgressEvent::LoaderFinished);
        }
    }

    let version = if let Some(patch) = patch {
        let instance = instance_recipe(game_version, instance_id, patch);
        repo.save_version_json(&instance)?;
        if !parent_was_visible {
            raw.hidden = Some(true);
            repo.save_version_json(&raw)?;
        }
        let mut versions = HashMap::new();
        versions.insert(game_version.to_string(), raw);
        versions.insert(instance_id.to_string(), instance.clone());
        instance.resolve(&versions)?
    } else if instance_id != game_version {
        let mut instance = Version::new(instance_id);
        instance.inherits_from = Some(game_version.to_string());
        repo.save_version_json(&instance)?;
        if !parent_was_visible {
            raw.hidden = Some(true);
            repo.save_version_json(&raw)?;
        }
        let mut versions = HashMap::new();
        versions.insert(game_version.to_string(), raw);
        versions.insert(instance_id.to_string(), instance.clone());
        instance.resolve(&versions)?
    } else {
        raw.hidden = Some(false);
        repo.save_version_json(&raw)?;
        raw.resolve(&HashMap::new())?
    };

    Ok(install::install_version_with_progress(
        client, provider, cache, repo, &version, env, progress,
    )
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn install_loader_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    instance_id: &str,
    selection: &LoaderSelection,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, GameInstallError> {
    let mut versions = repo.load_all_versions();
    let instance = versions
        .get(instance_id)
        .cloned()
        .ok_or_else(|| GameInstallError::MissingInstance(instance_id.to_string()))?;
    let resolved = instance.resolve(&versions)?;
    let game_version = game_version_of(&instance, &resolved);
    let vanilla = vanilla_for_installer(&versions, &instance, &game_version);

    if let Some(progress) = progress {
        let _ = progress.send(crate::download::ProgressEvent::LoaderStarted {
            name: selection.kind.display_name().to_string(),
        });
    }
    let patch = build_loader_patch(
        client,
        provider,
        cache,
        repo,
        game_dir,
        &game_version,
        &vanilla,
        selection,
    )
    .await?;
    if let Some(progress) = progress {
        let _ = progress.send(crate::download::ProgressEvent::LoaderFinished);
    }

    let updated = with_loader_patch(&instance, &game_version, patch);
    repo.save_version_json(&updated)?;
    versions.insert(instance_id.to_string(), updated.clone());
    let resolved = updated.resolve(&versions)?;
    Ok(install::install_version_with_progress(
        client, provider, cache, repo, &resolved, env, progress,
    )
    .await?)
}

pub fn remove_loader(
    repo: &GameRepository,
    instance_id: &str,
    kind: LoaderKind,
) -> Result<(), GameInstallError> {
    let mut instance = repo
        .load_all_versions()
        .remove(instance_id)
        .ok_or_else(|| GameInstallError::MissingInstance(instance_id.to_string()))?;
    let Some(patches) = instance.patches.as_mut() else {
        return Err(GameInstallError::LoaderNotInstalled(
            kind.display_name().to_string(),
        ));
    };
    let before = patches.len();
    patches.retain(|patch| patch.id != kind.slug());
    if patches.len() == before {
        return Err(GameInstallError::LoaderNotInstalled(
            kind.display_name().to_string(),
        ));
    }
    repo.save_version_json(&instance)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_instance_is_saved_as_a_parent_plus_patch_recipe() {
        let mut patch = Version::new("fabric");
        patch.version = Some("0.16.14".to_string());
        let instance = instance_recipe("1.20.1", "1.20.1-fabric", patch);

        assert_eq!(instance.inherits_from.as_deref(), Some("1.20.1"));
        assert_eq!(instance.patches.as_ref().unwrap()[0].id, "fabric");
        assert_eq!(
            instance.patches.as_ref().unwrap()[0].version.as_deref(),
            Some("0.16.14")
        );
    }

    #[test]
    fn instance_names_cannot_escape_the_versions_directory() {
        assert!(is_valid_instance_name("1.20.1-fabric"));
        assert!(!is_valid_instance_name("../outside"));
        assert!(!is_valid_instance_name("contains spaces"));
        assert!(!is_valid_instance_name(".."));
    }

    #[test]
    fn adding_a_loader_to_a_vanilla_instance_keeps_the_same_instance_id() {
        let mut vanilla = Version::new("1.20.1");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        let mut loader = Version::new("neoforge");
        loader.version = Some("20.1.1".to_string());
        loader.priority = Some(Version::PRIORITY_LOADER);
        loader.main_class = Some("cpw.mods.bootstraplauncher.BootstrapLauncher".to_string());

        let updated = with_loader_patch(&vanilla, "1.20.1", loader);
        let resolved = updated.resolve(&HashMap::new()).unwrap();

        assert_eq!(updated.id, "1.20.1");
        assert_eq!(updated.patches.as_ref().unwrap()[0].id, "game");
        assert_eq!(
            resolved.main_class.as_deref(),
            Some("cpw.mods.bootstraplauncher.BootstrapLauncher")
        );
        assert_eq!(resolved.jar.as_deref(), Some("1.20.1"));
        assert_eq!(
            crate::download::modrinth::detect_loader(&resolved),
            Some("neoforge")
        );
    }

    #[test]
    fn installing_another_loader_replaces_the_existing_loader_patch() {
        let mut vanilla = Version::new("1.20.1");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        let mut instance = Version::new("1.20.1-custom");
        instance.inherits_from = Some("1.20.1".to_string());
        instance.patches = Some(vec![Version::new("fabric")]);

        let updated = with_loader_patch(&instance, "1.20.1", Version::new("neoforge"));
        let mut provider = HashMap::new();
        provider.insert(vanilla.id.clone(), vanilla);
        provider.insert(updated.id.clone(), updated.clone());
        let resolved = updated.resolve(&provider).unwrap();
        let patches = updated.patches.unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].id, "neoforge");
        assert_eq!(
            crate::download::modrinth::detect_loader(&resolved),
            Some("neoforge")
        );
    }

    #[test]
    fn removing_a_loader_keeps_the_instance_and_its_game_dependency() {
        let root = std::env::temp_dir()
            .join("hmcl-rs-game-install-tests")
            .join(format!("remove-loader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = GameRepository::new(&root);
        let mut instance = Version::new("1.20.1-neoforge");
        instance.inherits_from = Some("1.20.1".to_string());
        instance.patches = Some(vec![Version::new("neoforge")]);
        repo.save_version_json(&instance).unwrap();

        remove_loader(&repo, &instance.id, LoaderKind::NeoForge).unwrap();

        let saved = repo.load_all_versions().remove(&instance.id).unwrap();
        assert_eq!(saved.inherits_from.as_deref(), Some("1.20.1"));
        assert!(saved.patches.unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

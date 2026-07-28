use std::path::Path;

use serde::Deserialize;

use crate::download::{CacheRepository, DownloadProvider};
use crate::install::{self, GameRepository};
use crate::version::{Artifact, Env, Library, Version};

use super::forge::{ForgeInstallError, InstallerArchive};

pub const PATCH_ID: &str = "forge";

#[derive(Debug, thiserror::Error)]
pub enum ForgeOldInstallError {
    #[error(transparent)]
    Forge(#[from] ForgeInstallError),
    #[error("old-format forge installer is missing its embedded universal jar at {0:?}")]
    MissingEmbeddedJar(String),
}

#[derive(Debug, Deserialize)]
struct ForgeInstallProfile {
    install: ForgeInstall,
    #[serde(rename = "versionInfo")]
    version_info: Version,
}

/// 对应 Java `ForgeInstall`。只建模我们真正用得上的两个字段——`profileName`/
/// `target`/`version`/`welcome`/`minecraft`/`mirrorList`/`logo` 在这条安装路径里
/// 从头到尾都用不上（`minecraft` 字段的版本匹配校验在更上层的分发逻辑里做，
/// 这里不重复读一遍）。
#[derive(Debug, Deserialize)]
struct ForgeInstall {
    path: Artifact,
    #[serde(rename = "filePath")]
    file_path: String,
}

/// 运行 Forge 旧版（<1.13）安装器，产出一个可以直接放进 `Version::patches` 的、
/// `priority = PRIORITY_LOADER` 的 patch。
///
/// 跟新版不一样，这里不需要 `vanilla_version`/`java_binary` 参数——旧版安装完全
/// 不碰原版 client.jar，也不需要跑任何外部程序。
pub async fn install_old_forge(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    installer_jar: &Path,
    self_version: &str,
) -> Result<Version, ForgeOldInstallError> {
    let mut installer = InstallerArchive::open(installer_jar)?;
    let profile: ForgeInstallProfile = installer.read_json("install_profile.json")?;

    let env = Env::current("");
    let forge_library = Library::from_artifact(profile.install.path.clone());
    let dest = repo.library_file(&forge_library, env);
    if !installer.copy_to(&profile.install.file_path, &dest)? {
        return Err(ForgeOldInstallError::MissingEmbeddedJar(
            profile.install.file_path,
        ));
    }

    // 补全 versionInfo.libraries 里其余的依赖(argo/guava 特定旧版本、asm 等)。
    // universal jar 本身通常也在这份 libraries 列表里, 但已经在上面抠出来放到位了,
    // fetch_to_file 的"文件已存在即跳过"会让它不会真的再发一次网络请求。
    install::install_libraries(client, provider, cache, repo, &profile.version_info, env).await;

    let mut patch = profile.version_info;
    patch.priority = Some(Version::PRIORITY_LOADER);
    patch.id = PATCH_ID.to_string();
    patch.version = Some(self_version.to_string());
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_jar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn install_profile_json_shape_matches_a_real_legacy_forge_installer() {
        const SAMPLE: &str = r#"{
            "install": {
                "profileName": "forge",
                "target": "1.7.10-Forge10.13.4.1614-1.7.10",
                "path": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
                "version": "Forge 10.13.4.1614",
                "filePath": "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar",
                "welcome": "Welcome to the simple forge installer.",
                "minecraft": "1.7.10",
                "mirrorList": "http://files.minecraftforge.net/mirror-brand.list",
                "logo": "/big_logo.png"
            },
            "versionInfo": {
                "id": "1.7.10-Forge10.13.4.1614-1.7.10",
                "time": "2020-01-01T00:00:00+0000",
                "releaseTime": "2020-01-01T00:00:00+0000",
                "type": "release",
                "minecraftArguments": "--username ${auth_player_name} --version ${version_name}",
                "mainClass": "net.minecraft.launchwrapper.Launch",
                "inheritsFrom": "1.7.10",
                "libraries": [
                    {"name": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10"},
                    {"name": "org.ow2.asm:asm-all:4.1"}
                ]
            }
        }"#;

        let profile: ForgeInstallProfile = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(
            profile.install.path.to_string(),
            "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10"
        );
        assert_eq!(
            profile.install.file_path,
            "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar"
        );
        assert_eq!(
            profile.version_info.main_class.as_deref(),
            Some("net.minecraft.launchwrapper.Launch")
        );
        assert!(
            profile.version_info.arguments.is_none(),
            "legacy versionInfo uses the minecraftArguments string form"
        );
        assert_eq!(profile.version_info.libraries.len(), 2);
    }

    #[tokio::test]
    async fn extracts_universal_jar_and_builds_a_loader_patch() {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-forge-old")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installer_path = dir.join("forge-installer.jar");
        let profile_json = r#"{
            "install": {
                "path": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
                "filePath": "the-universal.jar"
            },
            "versionInfo": {
                "id": "irrelevant-here",
                "mainClass": "net.minecraft.launchwrapper.Launch",
                "inheritsFrom": "1.7.10",
                "libraries": []
            }
        }"#;
        write_test_jar(
            &installer_path,
            &[
                ("install_profile.json", profile_json.as_bytes()),
                ("the-universal.jar", b"fake universal jar bytes"),
            ],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::mojang();
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();

        let patch = install_old_forge(
            &client,
            &provider,
            &cache,
            &repo,
            &installer_path,
            "10.13.4.1614",
        )
        .await
        .expect("legacy install must succeed with no network involved");

        assert_eq!(patch.id, PATCH_ID);
        assert_eq!(patch.version.as_deref(), Some("10.13.4.1614"));
        assert_eq!(patch.priority, Some(Version::PRIORITY_LOADER));
        assert_eq!(
            patch.main_class.as_deref(),
            Some("net.minecraft.launchwrapper.Launch")
        );

        let universal_dest = repo.library_file(
            &Library::from_artifact(
                Artifact::from_descriptor("net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10")
                    .unwrap(),
            ),
            Env::current(""),
        );
        assert_eq!(
            std::fs::read(&universal_dest).unwrap(),
            b"fake universal jar bytes",
            "universal jar must be extracted to the shared libraries directory"
        );
    }

    #[tokio::test]
    async fn missing_embedded_jar_is_reported_not_silently_ignored() {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-forge-old-missing")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installer_path = dir.join("forge-installer.jar");
        let profile_json = r#"{
            "install": {"path": "net.minecraftforge:forge:1.0", "filePath": "does-not-exist.jar"},
            "versionInfo": {"id": "x", "libraries": []}
        }"#;
        write_test_jar(
            &installer_path,
            &[("install_profile.json", profile_json.as_bytes())],
        );

        let root = dir.join("mc");
        let repo = GameRepository::new(&root);
        let provider = DownloadProvider::mojang();
        let cache = CacheRepository::new(root.join("cache"));
        let client = reqwest::Client::new();

        let err = install_old_forge(&client, &provider, &cache, &repo, &installer_path, "1.0")
            .await
            .unwrap_err();
        assert!(matches!(err, ForgeOldInstallError::MissingEmbeddedJar(_)));
    }
}

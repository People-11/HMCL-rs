use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use serde::Deserialize;

use crate::download::modrinth::ModrinthError;
use crate::download::{
    fetch_all_cached_with_progress, fetch_to_file_with_progress, modrinth, CacheRepository,
    DownloadProvider, Expected, FetchError, FetchJob, InstallStage, ProgressEvent, ProgressSink,
};
use crate::game_install::{is_valid_instance_name, GameInstallError, LoaderKind, LoaderSelection};
use crate::install::{GameRepository, InstallReport};
use crate::version::Env;

#[derive(Debug, thiserror::Error)]
pub enum ModpackError {
    #[error("failed to read .mrpack archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse modrinth.index.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    GameInstall(#[from] GameInstallError),
    #[error(transparent)]
    Modrinth(#[from] ModrinthError),
    #[error(transparent)]
    Version(#[from] crate::version::VersionError),
    #[error(".mrpack is missing modrinth.index.json")]
    MissingIndex,
    #[error("unsupported modpack format version {0} (only 1 is supported)")]
    UnsupportedFormatVersion(i64),
    #[error("modpack does not declare a minecraft version in its dependencies")]
    MissingGameVersion,
    #[error("invalid instance name {0:?}; use only ASCII letters, digits, '.', '-' and '_'")]
    InvalidInstanceName(String),
    #[error("instance {0} already exists")]
    InstanceAlreadyExists(String),
    #[error("a file inside the modpack archive has an unsafe path: {0}")]
    PathTraversal(String),
    #[error("cannot export instance {0}: it was not found")]
    InstanceNotFound(String),
    #[error("cannot export Modrinth modpack with unsupported loader {0}")]
    UnsupportedExportLoader(String),
}

#[derive(Debug, Clone, Deserialize)]
struct PackIndex {
    #[serde(rename = "formatVersion")]
    format_version: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    files: Vec<PackFile>,
    #[serde(default)]
    dependencies: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PackFile {
    path: String,
    #[serde(default)]
    hashes: PackFileHashes,
    #[serde(default)]
    env: Option<PackFileEnv>,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    file_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PackFileHashes {
    sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PackFileEnv {
    #[serde(default)]
    client: Option<String>,
}

impl PackFile {
    fn needed_on_client(&self) -> bool {
        !matches!(
            self.env.as_ref().and_then(|e| e.client.as_deref()),
            Some("unsupported")
        )
    }
}

const LOADER_DEPENDENCY_KEYS: &[(&str, LoaderKind)] = &[
    ("fabric-loader", LoaderKind::Fabric),
    ("quilt-loader", LoaderKind::Quilt),
    ("forge", LoaderKind::Forge),
    ("neoforge", LoaderKind::NeoForge),
];

fn detect_loader(dependencies: &HashMap<String, String>) -> Option<LoaderSelection> {
    LOADER_DEPENDENCY_KEYS.iter().find_map(|(key, kind)| {
        dependencies.get(*key).map(|version| LoaderSelection {
            kind: *kind,
            version: version.clone(),
        })
    })
}

/// 解压 `.mrpack` 里 `prefix`（`"overrides/"` 或 `"client-overrides/"`）下的所有
/// 文件到 `dest_root`——跟 `launch::process::unzip_native_library` 同一套路径
/// 穿越校验（压缩包本身就是不受信任的输入，条目名可以是任意字符串）。
fn extract_prefixed<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    prefix: &str,
    dest_root: &Path,
) -> Result<(), ModpackError> {
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        if relative.split('/').any(|seg| seg == "..") {
            return Err(ModpackError::PathTraversal(name));
        }

        let dest_file = dest_root.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest_file)?;
            continue;
        }
        if let Some(parent) = dest_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest_file)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn import_mrpack(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    mrpack_path: &Path,
    instance_id: &str,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, ModpackError> {
    if !is_valid_instance_name(instance_id) {
        return Err(ModpackError::InvalidInstanceName(instance_id.to_string()));
    }
    if repo.version_json_path(instance_id).is_file() {
        return Err(ModpackError::InstanceAlreadyExists(instance_id.to_string()));
    }

    let file = std::fs::File::open(mrpack_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    let index: PackIndex = {
        let mut entry = archive
            .by_name("modrinth.index.json")
            .map_err(|_| ModpackError::MissingIndex)?;
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        serde_json::from_str(&text)?
    };
    if index.format_version != 1 {
        return Err(ModpackError::UnsupportedFormatVersion(index.format_version));
    }
    let game_version = index
        .dependencies
        .get("minecraft")
        .ok_or(ModpackError::MissingGameVersion)?
        .clone();
    let loader = detect_loader(&index.dependencies);

    let report = crate::game_install::install_game_with_progress(
        client,
        provider,
        cache,
        repo,
        game_dir,
        &game_version,
        instance_id,
        loader.as_ref(),
        env,
        progress,
    )
    .await?;

    // 2. 标记这是个整合包实例——必须在算 run_directory 之前写，
    //    `GameRepository::run_directory` 靠这个文件是否存在判断要不要强制隔离
    //    （`run_directory_isolated` 对整合包无视 `runningDirectory` 设置，恒用
    //    `versions/{id}/` 自己的目录，见 install.rs）。
    std::fs::write(
        repo.version_root(instance_id).join("modpack.cfg"),
        format!("name={}\ntype=Modrinth\n", index.name),
    )?;
    let run_dir = repo.run_directory(instance_id);

    let jobs: Vec<FetchJob> = index
        .files
        .iter()
        .filter(|f| f.needed_on_client())
        .map(|f| {
            let dest = run_dir.join(&f.path);
            let expected = match &f.hashes.sha1 {
                Some(sha1) => Expected::sha1_and_size(sha1.clone(), f.file_size),
                None => Expected {
                    sha1: None,
                    size: Some(f.file_size).filter(|&s| s != 0),
                },
            };
            FetchJob {
                candidates: f.downloads.clone(),
                dest,
                expected,
            }
        })
        .collect();
    for (path, result) in fetch_all_cached_with_progress(
        client,
        cache,
        jobs,
        provider.concurrency(),
        progress.map(|tx| (InstallStage::ModpackFiles, tx)),
    )
    .await
    {
        result.map_err(|e| {
            tracing::warn!(?path, error = %e, "modpack file download failed");
            e
        })?;
    }

    // 4. overrides/ 先解压，client-overrides/ 后解压（客户端专属覆盖优先级更高）。
    extract_prefixed(&mut archive, "overrides/", &run_dir)?;
    extract_prefixed(&mut archive, "client-overrides/", &run_dir)?;

    Ok(report)
}

fn scratch_download_dir(game_dir: &Path) -> std::path::PathBuf {
    game_dir.join(".hmcl-rs-cache").join("modpack-downloads")
}

#[allow(clippy::too_many_arguments)]
pub async fn import_from_url(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    download_url: &str,
    instance_id: &str,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, ModpackError> {
    let scratch = scratch_download_dir(game_dir);
    std::fs::create_dir_all(&scratch)?;
    let dest = scratch.join(format!("{instance_id}.mrpack"));
    if let Some(tx) = progress {
        let _ = tx.send(ProgressEvent::StageStarted {
            stage: InstallStage::ModpackArchive,
            total: 1,
        });
    }
    let download_result = fetch_to_file_with_progress(
        client,
        &provider.inject_url_candidates(download_url),
        &dest,
        &Expected::default(),
        |chunk_bytes| {
            if let Some(tx) = progress {
                let _ = tx.send(ProgressEvent::Bytes {
                    path: dest.clone(),
                    chunk_bytes,
                    total_bytes: None,
                });
            }
        },
    )
    .await;
    if let Some(tx) = progress {
        let _ = tx.send(ProgressEvent::TaskDone {
            stage: InstallStage::ModpackArchive,
        });
    }
    download_result?;

    let result = import_mrpack(
        client,
        provider,
        cache,
        repo,
        game_dir,
        &dest,
        instance_id,
        env,
        progress,
    )
    .await;
    let _ = std::fs::remove_file(&dest);
    result
}

/// HMCL"安装整合包"的第三种来源——从 Modrinth 在线搜索结果直接装, 不用户自己
/// 找下载链接：先用 [`modrinth::install_latest_compatible`] 把这个整合包项目
/// 最新兼容版本的 `.mrpack` 下载下来（不按游戏版本/加载器过滤——整合包本身的
/// 游戏版本/加载器写在它自己的 `modrinth.index.json` 里, 用户在这一步还没有
/// "目标实例"这个概念可供过滤), 再走跟本地文件一样的导入路径。CurseForge 的
/// 不处理服务端专用文件。
#[allow(clippy::too_many_arguments)]
pub async fn import_from_modrinth(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    project_id: &str,
    instance_id: &str,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, ModpackError> {
    let scratch = scratch_download_dir(game_dir);
    let downloaded = modrinth::install_latest_compatible(
        client, provider, cache, project_id, None, None, &scratch,
    )
    .await?;

    let result = import_mrpack(
        client,
        provider,
        cache,
        repo,
        game_dir,
        &downloaded,
        instance_id,
        env,
        progress,
    )
    .await;
    let _ = std::fs::remove_file(&downloaded);
    result
}

#[allow(clippy::too_many_arguments)]
pub async fn import_from_modrinth_version(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &CacheRepository,
    repo: &GameRepository,
    game_dir: &Path,
    version: &modrinth::ProjectVersion,
    instance_id: &str,
    env: Env<'_>,
    progress: Option<&ProgressSink>,
) -> Result<InstallReport, ModpackError> {
    let scratch = scratch_download_dir(game_dir);
    let file = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
        .ok_or(ModrinthError::NoCompatibleVersion)?;
    if let Some(tx) = progress {
        let _ = tx.send(ProgressEvent::StageStarted {
            stage: InstallStage::ModpackArchive,
            total: 1,
        });
    }
    let download_path = scratch.join(&file.filename);
    let downloaded = modrinth::install_version_file_as_with_progress(
        client,
        provider,
        cache,
        version,
        &scratch,
        &file.filename,
        |chunk_bytes| {
            if let Some(tx) = progress {
                let _ = tx.send(ProgressEvent::Bytes {
                    path: download_path.clone(),
                    chunk_bytes,
                    total_bytes: Some(file.size).filter(|&size| size != 0),
                });
            }
        },
    )
    .await;
    if let Some(tx) = progress {
        let _ = tx.send(ProgressEvent::TaskDone {
            stage: InstallStage::ModpackArchive,
        });
    }
    let downloaded = downloaded?;
    let result = import_mrpack(
        client,
        provider,
        cache,
        repo,
        game_dir,
        &downloaded,
        instance_id,
        env,
        progress,
    )
    .await;
    let _ = std::fs::remove_file(&downloaded);
    result
}

const EXPORT_BLACKLIST: &[&str] = &[
    ".hmcl-rs-cache",
    "assets",
    "backups",
    "crash-reports",
    "libraries",
    "logs",
    "versions",
];

fn exportable_path(relative: &Path, instance_id: &str) -> bool {
    let Some(first) = relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
    else {
        return false;
    };
    if EXPORT_BLACKLIST
        .iter()
        .any(|blocked| first.eq_ignore_ascii_case(blocked))
        || first.starts_with("natives-")
    {
        return false;
    }
    let relative_text = relative.to_string_lossy().replace('\\', "/");
    !matches!(
        relative_text.as_str(),
        "launcher_profiles.json"
            | "launcher_accounts.json"
            | "usercache.json"
            | "usernamecache.json"
            | "modpack.cfg"
            | "instance-game-settings.json"
    ) && relative_text != format!("{instance_id}.jar")
        && relative_text != format!("{instance_id}.json")
}

fn add_override_files(
    writer: &mut zip::ZipWriter<std::fs::File>,
    root: &Path,
    directory: &Path,
    output: &Path,
    instance_id: &str,
) -> Result<(), ModpackError> {
    let mut entries: Vec<_> = std::fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path == output {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ModpackError::PathTraversal(path.display().to_string()))?;
        if !exportable_path(relative, instance_id) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            add_override_files(writer, root, &path, output, instance_id)?;
        } else if entry.file_type()?.is_file() {
            let name = format!(
                "client-overrides/{}",
                relative.to_string_lossy().replace('\\', "/")
            );
            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(name, options)?;
            std::io::copy(&mut std::fs::File::open(path)?, writer)?;
        }
    }
    Ok(())
}

fn export_dependencies(
    version: &crate::version::Version,
) -> Result<HashMap<String, String>, ModpackError> {
    let mut dependencies = HashMap::from([(
        "minecraft".to_string(),
        modrinth::detect_game_version(version).to_string(),
    )]);
    if let Some(loader) = modrinth::detect_loader(version) {
        let key = match loader {
            "fabric" => "fabric-loader",
            "quilt" => "quilt-loader",
            "forge" => "forge",
            "neoforge" => "neoforge",
            unsupported => {
                return Err(ModpackError::UnsupportedExportLoader(
                    unsupported.to_string(),
                ))
            }
        };
        let loader_version = version
            .patches
            .as_ref()
            .and_then(|patches| patches.iter().find(|patch| patch.id == loader))
            .and_then(|patch| patch.version.clone())
            .ok_or_else(|| {
                ModpackError::UnsupportedExportLoader(format!("{loader}（缺少版本号）"))
            })?;
        dependencies.insert(key.to_string(), loader_version);
    }
    Ok(dependencies)
}

pub fn export_mrpack(
    repo: &GameRepository,
    instance_id: &str,
    output: &Path,
    name: &str,
    version_id: &str,
    summary: &str,
) -> Result<(), ModpackError> {
    let all = repo.load_all_versions();
    let raw = all
        .get(instance_id)
        .ok_or_else(|| ModpackError::InstanceNotFound(instance_id.to_string()))?;
    let resolved = raw.resolve(&all)?;
    let dependencies = export_dependencies(&resolved)?;
    let run_dir = repo.run_directory(instance_id);
    std::fs::create_dir_all(output.parent().unwrap_or_else(|| Path::new(".")))?;
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };

    let file = std::fs::File::create(&output)?;
    let mut writer = zip::ZipWriter::new(file);
    if run_dir.is_dir() {
        add_override_files(&mut writer, &run_dir, &run_dir, &output, instance_id)?;
    }
    let manifest = serde_json::json!({
        "game": "minecraft",
        "formatVersion": 1,
        "versionId": version_id,
        "name": name,
        "summary": summary,
        "files": [],
        "dependencies": dependencies,
    });
    let options: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("modrinth.index.json", options)?;
    writer.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_loader_dependency_keys() {
        let mut deps = HashMap::new();
        deps.insert("minecraft".to_string(), "1.20.1".to_string());
        deps.insert("fabric-loader".to_string(), "0.16.14".to_string());
        let loader = detect_loader(&deps).unwrap();
        assert_eq!(loader.kind, LoaderKind::Fabric);
        assert_eq!(loader.version, "0.16.14");

        let vanilla_only: HashMap<String, String> =
            [("minecraft".to_string(), "1.20.1".to_string())].into();
        assert!(detect_loader(&vanilla_only).is_none());
    }

    #[test]
    fn client_env_unsupported_files_are_excluded_others_are_kept() {
        let server_only = PackFile {
            path: "mods/server-plugin.jar".to_string(),
            hashes: PackFileHashes::default(),
            env: Some(PackFileEnv {
                client: Some("unsupported".to_string()),
            }),
            downloads: vec![],
            file_size: 0,
        };
        assert!(!server_only.needed_on_client());

        let optional = PackFile {
            path: "mods/optional.jar".to_string(),
            hashes: PackFileHashes::default(),
            env: Some(PackFileEnv {
                client: Some("optional".to_string()),
            }),
            downloads: vec![],
            file_size: 0,
        };
        assert!(optional.needed_on_client());

        let no_env = PackFile {
            path: "mods/required.jar".to_string(),
            hashes: PackFileHashes::default(),
            env: None,
            downloads: vec![],
            file_size: 0,
        };
        assert!(no_env.needed_on_client());
    }

    #[test]
    fn parses_a_real_shaped_index_json() {
        let json = r#"{
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1.0.0",
            "name": "Example Pack",
            "files": [
                {
                    "path": "mods/sodium.jar",
                    "hashes": {"sha1": "abc123", "sha512": "def456"},
                    "env": {"client": "required", "server": "unsupported"},
                    "downloads": ["https://cdn.modrinth.com/data/AANobbMI/versions/x/sodium.jar"],
                    "fileSize": 1234
                }
            ],
            "dependencies": {"minecraft": "1.20.1", "fabric-loader": "0.16.14"}
        }"#;
        let index: PackIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.format_version, 1);
        assert_eq!(index.dependencies.get("minecraft").unwrap(), "1.20.1");
        assert_eq!(index.files.len(), 1);
        assert_eq!(index.files[0].hashes.sha1.as_deref(), Some("abc123"));
        assert!(index.files[0].needed_on_client());
    }

    #[tokio::test]
    async fn import_rejects_an_instance_name_that_already_exists() {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test")
            .join("modpack_import_dup")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = GameRepository::new(&dir);
        repo.save_version_json(&crate::version::Version::new("existing"))
            .unwrap();

        let client = reqwest::Client::new();
        let provider = DownloadProvider::mojang();
        let cache = CacheRepository::new(dir.join("cache"));
        let env = Env {
            platform: crate::platform::Platform::CURRENT,
            os_version: "",
        };

        let err = import_mrpack(
            &client,
            &provider,
            &cache,
            &repo,
            &dir,
            Path::new("does-not-matter.mrpack"),
            "existing",
            env,
            None,
        )
        .await
        .expect_err("must reject a name collision before even opening the archive");
        assert!(matches!(err, ModpackError::InstanceAlreadyExists(_)));
    }

    #[test]
    fn export_writes_importable_manifest_and_only_instance_files() {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test")
            .join("modpack_export")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = GameRepository::new(&dir);
        let mut vanilla = crate::version::Version::new("1.20.1");
        vanilla.jar = Some("1.20.1".to_string());
        repo.save_version_json(&vanilla).unwrap();
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods").join("example.jar"), b"mod").unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs").join("latest.log"), b"log").unwrap();

        let output = dir.join("example.mrpack");
        export_mrpack(&repo, "1.20.1", &output, "Example", "1.0", "Summary").unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&output).unwrap()).unwrap();
        assert!(archive.by_name("client-overrides/mods/example.jar").is_ok());
        assert!(archive.by_name("client-overrides/logs/latest.log").is_err());
        let manifest: serde_json::Value =
            serde_json::from_reader(archive.by_name("modrinth.index.json").unwrap()).unwrap();
        assert_eq!(manifest["dependencies"]["minecraft"], "1.20.1");
        assert_eq!(manifest["name"], "Example");
    }
}

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::{stream, StreamExt};
use serde::Deserialize;

use super::{DownloadProvider, Expected, FetchError};
use crate::java::{JavaInfo, JavaInfoError, JavaRuntime};
use crate::platform::{Architecture, OperatingSystem, Platform};
use crate::version::DownloadInfo;

const JAVA_LIST_URL: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MojangJavaComponent {
    /// Java 8——1.13 以下版本用的老运行时，也是修好老版本 Forge/LiteLoader 的
    /// `LaunchWrapper` 假设系统类加载器是 `URLClassLoader` 这条硬约束所需要的那个。
    JreLegacy,
    RuntimeAlpha,   // Java 16
    RuntimeBeta,    // Java 17
    RuntimeDelta,   // Java 21
    RuntimeEpsilon, // Java 25
}

#[derive(Debug, Clone)]
pub struct MojangJavaProgress {
    pub path: String,
    pub completed_files: usize,
    pub total_files: usize,
    pub downloaded: u64,
    pub total_bytes: u64,
    pub finished: bool,
}

impl MojangJavaComponent {
    pub fn component_key(self) -> &'static str {
        match self {
            MojangJavaComponent::JreLegacy => "jre-legacy",
            MojangJavaComponent::RuntimeAlpha => "java-runtime-alpha",
            MojangJavaComponent::RuntimeBeta => "java-runtime-beta",
            MojangJavaComponent::RuntimeDelta => "java-runtime-delta",
            MojangJavaComponent::RuntimeEpsilon => "java-runtime-epsilon",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MojangJavaError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse mojang java manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    JavaInfo(#[from] JavaInfoError),
    #[error("mojang does not publish a java runtime for this platform")]
    UnsupportedPlatform,
    #[error("no {0} build is listed for this platform")]
    NoBuildForComponent(&'static str),
    #[error("manifest entry {0:?} is a symlink, which this port does not support installing")]
    UnsupportedSymlink(String),
    #[error("file entry {0:?} has no \"raw\" download variant, which is the only one this port implements")]
    NoRawDownload(String),
    #[error("installed runtime is missing its release file")]
    MissingReleaseFile,
}

/// 对应 Java `JavaManager.getMojangJavaPlatform`：Mojang 这份清单自己的平台命名
/// (`"windows-x64"`)，跟我们自己文件系统用的 `Platform::Display`
/// (`"windows-x86_64"`) 是两个完全不同的字符串——前者只用来在这份清单里查找条目，
/// 后者才是本地安装目录用的名字，不要混用。
///
/// ponytail: Java 版这个方法覆盖 Windows/Linux/macOS 全平台 x86/x86_64/arm64 的
/// 完整映射表；这里只搬 Windows x86_64（HMCL-rs 唯一目标平台）这一条。
fn mojang_platform_key(platform: Platform) -> Option<&'static str> {
    match (platform.os, platform.arch) {
        (OperatingSystem::Windows, Architecture::X86_64) => Some("windows-x64"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct VersionName {
    #[allow(dead_code)] // 只用来在真实清单里核对/调试, 选构建时目前直接取第一条。
    name: String,
}

#[derive(Debug, Deserialize)]
struct JavaBuild {
    manifest: DownloadInfo,
    #[allow(dead_code)] // 只用来在真实清单里核对/调试, 选构建时目前直接取第一条。
    version: VersionName,
}

type AllJavaManifest = HashMap<String, HashMap<String, Vec<JavaBuild>>>;

#[derive(Debug, Deserialize)]
struct RemoteFilesManifest {
    files: HashMap<String, RemoteEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RemoteEntry {
    File {
        #[serde(default)]
        #[allow(dead_code)]
        executable: bool,
        downloads: HashMap<String, DownloadInfo>,
    },
    Directory,
    Link {
        #[allow(dead_code)]
        target: String,
    },
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    candidates: &[String],
) -> Result<T, MojangJavaError> {
    let mut last_err = None;
    for url in candidates {
        match client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
        {
            Ok(resp) => match resp.text().await {
                Ok(text) => return Ok(serde_json::from_str(&text)?),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("candidates is non-empty").into())
}

pub async fn install_mojang_java(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    install_root: &Path,
    component: MojangJavaComponent,
) -> Result<JavaRuntime, MojangJavaError> {
    install_mojang_java_with_progress(client, provider, install_root, component, |_| {}).await
}

pub async fn install_mojang_java_with_progress(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    install_root: &Path,
    component: MojangJavaComponent,
    on_progress: impl Fn(MojangJavaProgress) + Clone,
) -> Result<JavaRuntime, MojangJavaError> {
    let platform = Platform::CURRENT;
    let platform_key = mojang_platform_key(platform).ok_or(MojangJavaError::UnsupportedPlatform)?;

    let all: AllJavaManifest =
        fetch_json(client, &provider.inject_url_candidates(JAVA_LIST_URL)).await?;
    let build = all
        .get(platform_key)
        .and_then(|components| components.get(component.component_key()))
        .and_then(|builds| builds.first())
        .ok_or(MojangJavaError::NoBuildForComponent(
            component.component_key(),
        ))?;

    let manifest_url =
        build
            .manifest
            .url
            .as_deref()
            .ok_or(MojangJavaError::NoBuildForComponent(
                component.component_key(),
            ))?;
    let files_manifest: RemoteFilesManifest =
        fetch_json(client, &provider.inject_url_candidates(manifest_url)).await?;

    let install_dir = install_root
        .join(platform.to_string())
        .join(format!("mojang-{}", component.component_key()));
    std::fs::create_dir_all(&install_dir)?;
    let mut jobs = Vec::new();
    for (rel_path, entry) in files_manifest.files {
        let dest = install_dir.join(&rel_path);
        match entry {
            RemoteEntry::Directory => {
                std::fs::create_dir_all(&dest)?;
            }
            RemoteEntry::Link { .. } => {
                return Err(MojangJavaError::UnsupportedSymlink(rel_path));
            }
            RemoteEntry::File { downloads, .. } => {
                let raw = downloads
                    .get("raw")
                    .ok_or_else(|| MojangJavaError::NoRawDownload(rel_path.clone()))?;
                let url = raw
                    .url
                    .as_deref()
                    .ok_or_else(|| MojangJavaError::NoRawDownload(rel_path.clone()))?;
                let expected = Expected {
                    sha1: raw.sha1.clone(),
                    size: if raw.size > 0 { Some(raw.size) } else { None },
                };
                jobs.push((
                    rel_path,
                    dest,
                    provider.inject_url_candidates(url),
                    expected,
                    raw.size,
                ));
            }
        }
    }

    let total_files = jobs.len();
    let completed_files = Arc::new(AtomicUsize::new(0));
    let results = stream::iter(jobs)
        .map(|(path, dest, candidates, expected, total_bytes)| {
            let client = client.clone();
            let completed_files = completed_files.clone();
            let on_progress = on_progress.clone();
            async move {
                let mut downloaded = 0;
                on_progress(MojangJavaProgress {
                    path: path.clone(),
                    completed_files: completed_files.load(Ordering::Relaxed),
                    total_files,
                    downloaded,
                    total_bytes,
                    finished: false,
                });
                super::fetch_to_file_with_progress(
                    &client,
                    &candidates,
                    &dest,
                    &expected,
                    |chunk| {
                        downloaded += chunk;
                        on_progress(MojangJavaProgress {
                            path: path.clone(),
                            completed_files: completed_files.load(Ordering::Relaxed),
                            total_files,
                            downloaded,
                            total_bytes,
                            finished: false,
                        });
                    },
                )
                .await?;
                let completed = completed_files.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(MojangJavaProgress {
                    path,
                    completed_files: completed,
                    total_files,
                    downloaded: total_bytes,
                    total_bytes,
                    finished: true,
                });
                // ponytail: 不处理 Unix 可执行位——HMCL-rs 只面向 Windows(windows-gnu),
                // Windows 没有这个概念, `executable` 字段在这个平台上天生就是无意义的。
                Ok::<_, MojangJavaError>(())
            }
        })
        .buffer_unordered(provider.concurrency())
        .collect::<Vec<_>>()
        .await;
    for result in results {
        result?;
    }

    let release_text = tokio::fs::read_to_string(install_dir.join("release"))
        .await
        .map_err(|_| MojangJavaError::MissingReleaseFile)?;
    let info = JavaInfo::from_release_file(&release_text)?;
    let java_exe = install_dir.join("bin").join("java.exe");
    Ok(JavaRuntime::of(java_exe, info, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE_ALL_JSON: &str = r#"{
        "windows-x64": {
            "jre-legacy": [
                {"availability": {"group": 4030, "progress": 100}, "manifest": {"sha1": "abc", "size": 80031, "url": "MANIFEST_URL"}, "version": {"name": "8u51-cacert462b08", "released": "2025-10-06T13:49:59+00:00"}}
            ]
        }
    }"#;

    #[test]
    fn parses_real_shaped_all_json() {
        let all: AllJavaManifest = serde_json::from_str(
            &SAMPLE_ALL_JSON.replace("MANIFEST_URL", "https://example.com/manifest.json"),
        )
        .unwrap();
        let build = all
            .get("windows-x64")
            .unwrap()
            .get("jre-legacy")
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(build.version.name, "8u51-cacert462b08");
        assert_eq!(
            build.manifest.url.as_deref(),
            Some("https://example.com/manifest.json")
        );
    }

    #[test]
    fn parses_real_shaped_per_file_manifest_with_tagged_entry_types() {
        const SAMPLE: &str = r#"{
            "files": {
                "bin/java.exe": {"type": "file", "executable": true, "downloads": {"raw": {"url": "https://example.com/java.exe", "sha1": "x", "size": 1}, "lzma": {"url": "https://example.com/java.exe.lzma", "sha1": "y", "size": 1}}},
                "LICENSE": {"type": "file", "executable": false, "downloads": {"raw": {"url": "https://example.com/LICENSE", "sha1": "z", "size": 40}}},
                "lib": {"type": "directory"},
                "some/symlink": {"type": "link", "target": "../other"}
            }
        }"#;
        let manifest: RemoteFilesManifest = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(manifest.files.len(), 4);
        assert!(matches!(
            manifest.files.get("lib"),
            Some(RemoteEntry::Directory)
        ));
        assert!(matches!(
            manifest.files.get("some/symlink"),
            Some(RemoteEntry::Link { .. })
        ));
        match manifest.files.get("bin/java.exe") {
            Some(RemoteEntry::File { downloads, .. }) => {
                assert!(downloads.contains_key("raw") && downloads.contains_key("lzma"))
            }
            other => panic!("expected a file entry, got {other:?}"),
        }
    }

    #[test]
    fn mojang_platform_key_only_recognizes_windows_x86_64() {
        assert_eq!(
            mojang_platform_key(Platform::WINDOWS_X64),
            Some("windows-x64")
        );
        assert_eq!(
            mojang_platform_key(Platform::LINUX_X64),
            None,
            "ponytail scope: only windows-gnu x86_64 is supported"
        );
    }

    #[tokio::test]
    async fn installs_a_minimal_fake_jre_end_to_end() {
        let server = MockServer::start().await;
        let release_body = b"JAVA_VERSION=\"8u51\"\nOS_NAME=\"Windows\"\nOS_ARCH=\"x86_64\"\nIMPLEMENTOR=\"Oracle Corporation\"\n".to_vec();
        let java_exe_body = b"fake java.exe bytes".to_vec();

        Mock::given(method("GET"))
            .and(path(
                "/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                SAMPLE_ALL_JSON.replace("MANIFEST_URL", &format!("{}/manifest.json", server.uri())),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": {
                    "release": {"type": "file", "executable": false, "downloads": {"raw": {"url": format!("{}/release", server.uri()), "sha1": crate::download::fetch::sha1_hex(&release_body), "size": release_body.len()}}},
                    "bin": {"type": "directory"},
                    "bin/java.exe": {"type": "file", "executable": true, "downloads": {"raw": {"url": format!("{}/java.exe", server.uri()), "sha1": crate::download::fetch::sha1_hex(&java_exe_body), "size": java_exe_body.len()}}}
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/release"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(30))
                    .set_body_bytes(release_body.clone()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/java.exe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(30))
                    .set_body_bytes(java_exe_body.clone()),
            )
            .mount(&server)
            .await;

        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-mojang-java")
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let provider = DownloadProvider::bmclapi(server.uri()).with_concurrency(2);
        let client = reqwest::Client::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_events = events.clone();

        let runtime = install_mojang_java_with_progress(
            &client,
            &provider,
            &dir,
            MojangJavaComponent::JreLegacy,
            move |progress| progress_events.lock().unwrap().push(progress),
        )
        .await
        .expect("install should succeed against the mock server");

        assert!(runtime.is_managed);
        assert_eq!(runtime.info.parsed_major_version(), Some(8));
        assert_eq!(
            tokio::fs::read(&runtime.binary).await.unwrap(),
            java_exe_body
        );
        let events = events.lock().unwrap();
        let first_finished = events.iter().position(|event| event.finished).unwrap();
        assert_eq!(
            events[..first_finished]
                .iter()
                .map(|event| event.path.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2,
            "the configured concurrency must start both files before either finishes"
        );
    }
}

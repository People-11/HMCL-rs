use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncBufReadExt;

use crate::install::GameRepository;
use crate::platform::Platform;
use crate::version::{Env, Library, Version};

use super::{GeneratedCommand, LaunchOptions};

#[derive(Debug, thiserror::Error)]
pub enum NativesError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("zip entry escapes destination directory: {0}")]
    PathTraversal(String),
}

/// 对应 Java `DefaultLauncher.decompressNatives`。
///
/// ponytail: Java 版的 filter 里有一条"目标文件已存在且大小和 zip 里的条目一致就
/// 跳过"的检查，但紧接着实际拷贝那一步（`Unzipper` 在 `replaceExistentFile=false`,
/// `decompressNatives` 恒传这个值）遇到任何已存在的目标文件都会静默跳过拷贝
/// （`Files.copy` 不带 `REPLACE_EXISTING`，抛 `FileAlreadyExistsException` 被吞掉）——
/// 也就是说不管大小对不对，已存在就不会被覆盖。两层检查叠在一起，第一层的大小
/// 比较对最终行为没有任何影响（真正生效的只有"存在即跳过"）。这里直接实现
/// "存在即跳过"这一条，效果和 Java 完全一致，不是抄漏了。
pub fn decompress_natives(
    version: &Version,
    repo: &GameRepository,
    destination: &Path,
    platform: Platform,
    use_native_glfw: bool,
    use_native_openal: bool,
) -> Result<(), NativesError> {
    let _ = std::fs::remove_dir_all(destination); // cleanDirectoryQuietly: 尽力而为, 忽略错误
    std::fs::create_dir_all(destination)?;

    let env = Env {
        platform,
        os_version: "",
    };

    for lib in &version.libraries {
        if !lib.is_native(env) {
            continue;
        }
        let jar_path = repo.library_file(lib, env);
        if !jar_path.is_file() {
            continue; // 缺失的 native 库跳过, 和 classpath 生成一样"缺了就缺了"的哲学
        }
        unzip_native_library(
            &jar_path,
            destination,
            lib,
            use_native_glfw,
            use_native_openal,
        )?;
    }

    Ok(())
}

fn unzip_native_library(
    jar_path: &Path,
    destination: &Path,
    library: &Library,
    use_native_glfw: bool,
    use_native_openal: bool,
) -> Result<(), NativesError> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let extract_rules = library.extract();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let relative_path = entry.name().replace('\\', "/");

        if relative_path.split('/').any(|seg| seg == "..") {
            return Err(NativesError::PathTraversal(relative_path));
        }

        let dest_file = destination.join(&relative_path);

        if entry.is_dir() {
            std::fs::create_dir_all(&dest_file)?;
            continue;
        }

        let ext = dest_file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "sha1" || ext == "git" {
            continue;
        }

        let file_name_lower = dest_file
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if use_native_glfw && file_name_lower.contains("glfw") {
            continue;
        }
        if use_native_openal && file_name_lower.contains("openal") {
            continue;
        }

        if !extract_rules.should_extract(&relative_path) {
            continue;
        }

        if dest_file.exists() {
            continue; // replaceExistentFile=false: 已存在就不碰, 见函数头注释
        }

        if let Some(parent) = dest_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest_file)?;
        std::io::copy(&mut entry, &mut out)?;
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessLaunchError {
    #[error(transparent)]
    Natives(#[from] NativesError),
    #[error("failed to spawn process: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("background natives-extraction task panicked: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// 对应 Java `ManagedProcess`（去掉了 `properties`/`lines`/`relatedThreads` 那套
/// 给 JavaFX UI 用的状态——这里就是薄薄一层 `tokio::process::Child` 包装）。
pub struct ManagedProcess {
    pub child: tokio::process::Child,
    pub commands: Vec<String>,
}

impl ManagedProcess {
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    pub fn stop(&mut self) -> std::io::Result<()> {
        self.child.start_kill()
    }
}

fn env_vars(
    repo: &GameRepository,
    version: &Version,
    options: &LaunchOptions,
) -> Vec<(String, String)> {
    let version_name = options
        .version_name
        .clone()
        .unwrap_or_else(|| version.id.clone());
    vec![
        ("INST_NAME".to_string(), version_name.clone()),
        ("INST_ID".to_string(), version_name),
        (
            "INST_DIR".to_string(),
            repo.version_root(&version.id)
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "INST_MC_DIR".to_string(),
            repo.run_directory(&version.id)
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "INST_JAVA".to_string(),
            options.java.binary.to_string_lossy().into_owned(),
        ),
    ]
}

/// 对应 Java `Launcher.launch()`：解压 natives、拼环境变量、起子进程。
///
/// `generated` 必须是用同一个 `native_folder` 调 [`super::generate_command_line`]
/// 产出的（`generated.java_native_folder` 和实际解压目标目录要对得上，否则
/// `-Djava.library.path` 指向的地方跟真正解压 natives 的地方就不是同一个目录了）。
pub async fn launch(
    repo: &GameRepository,
    version: &Version,
    options: &LaunchOptions,
    generated: GeneratedCommand,
) -> Result<ManagedProcess, ProcessLaunchError> {
    if !options.use_custom_natives {
        let version = version.clone();
        let repo_root = repo.root.clone();
        let platform = options.java.info.platform;
        let use_glfw = options.use_native_glfw;
        let use_openal = options.use_native_openal;
        let native_folder = generated.java_native_folder.clone();
        tokio::task::spawn_blocking(move || {
            let repo = GameRepository::new(repo_root);
            decompress_natives(
                &version,
                &repo,
                &native_folder,
                platform,
                use_glfw,
                use_openal,
            )
        })
        .await??;
    }

    let args = generated.command.as_list();
    if args.is_empty() || args.iter().any(|a| a.trim().is_empty()) {
        return Err(ProcessLaunchError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("illegal command line: {args:?}"),
        )));
    }

    let run_directory = repo.run_directory(&version.id);

    let mut cmd = tokio::process::Command::new(&args[0]);
    crate::platform::hide_console_window(&mut cmd);
    cmd.args(&args[1..]);
    cmd.current_dir(&run_directory);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    if let Some(appdata) = options.game_dir.to_path_buf().parent() {
        cmd.env("APPDATA", appdata);
    }
    for (k, v) in env_vars(repo, version, options) {
        cmd.env(k, v);
    }
    for (k, v) in &options.extra_environment_variables {
        cmd.env(k, v);
    }

    let child = cmd.spawn()?;
    Ok(ManagedProcess {
        child,
        commands: args,
    })
}

/// 逐行读子进程的 stdout/stderr 并回调。用 UTF-8 解码是有意为之，不是图省事：
/// 命令行生成阶段已经通过 `-Dstdout.encoding=UTF-8`/`-Dsun.stdout.encoding=UTF-8`
/// 强制 JVM 用 UTF-8 输出（见 `launch::native_charset_name` 的说明），子进程的
/// 字节流本来就是 UTF-8，不需要再按 Windows 系统代码页
/// （Java 版 `OperatingSystem.NATIVE_CHARSET`，中文系统上是 GBK）解码。
/// `AsyncBufReadExt::lines()` 内部就是按 UTF-8（有损）解码的，正好符合这个前提。
pub async fn pump_lines<R>(stream: R, mut on_line: impl FnMut(String)) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = tokio::io::BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        on_line(line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::java::{JavaInfo, JavaRuntime};
    use crate::launch::CommandBuilder;
    use crate::platform::{Architecture, OperatingSystem};
    use crate::version::{
        Artifact, CompatibilityRule, ExtractRules, LibrariesDownloadInfo, LibraryDownloadInfo,
        OsRestriction, RuleAction,
    };
    use std::collections::HashMap;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-natives")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    fn native_library(artifact_descriptor: &str) -> Library {
        Library {
            artifact: Artifact::from_descriptor(artifact_descriptor).unwrap(),
            url: None,
            downloads: Some(LibrariesDownloadInfo {
                artifact: None,
                classifiers: HashMap::from([(
                    "natives-windows".to_string(),
                    LibraryDownloadInfo {
                        path: None,
                        download: Default::default(),
                    },
                )]),
            }),
            extract: Some(ExtractRules {
                exclude: vec!["META-INF/".to_string()],
            }),
            natives: Some(HashMap::from([(
                "windows".to_string(),
                "natives-windows".to_string(),
            )])),
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }
    }

    #[test]
    fn extracts_native_jar_and_respects_extract_rules_and_filters() {
        let dir = tmp_dir("extract_basic");
        let jar_path = dir.join("lib.jar");
        write_test_jar(
            &jar_path,
            &[
                ("lwjgl.dll", b"fake dll bytes"),
                ("lwjgl.dll.sha1", b"deadbeef"),
                (
                    "META-INF/MANIFEST.MF",
                    b"should be excluded by ExtractRules",
                ),
                ("openal32.dll", b"fake openal"),
            ],
        );

        let dest = dir.join("natives");
        let lib = native_library("org.lwjgl:lwjgl:3.3.1");

        unzip_native_library(&jar_path, &dest, &lib, false, true).unwrap();

        assert!(
            dest.join("lwjgl.dll").is_file(),
            "regular native file must be extracted"
        );
        assert!(
            !dest.join("lwjgl.dll.sha1").exists(),
            ".sha1 files must be filtered out"
        );
        assert!(
            !dest.join("META-INF/MANIFEST.MF").exists(),
            "ExtractRules exclude must be honored"
        );
        assert!(
            !dest.join("openal32.dll").exists(),
            "use_native_openal=true must filter out openal files"
        );
    }

    #[test]
    fn does_not_overwrite_an_already_extracted_file() {
        let dir = tmp_dir("no_overwrite");
        let jar_path = dir.join("lib.jar");
        write_test_jar(
            &jar_path,
            &[("lwjgl.dll", b"new content, much longer than before")],
        );

        let dest = dir.join("natives");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("lwjgl.dll"), b"stale content").unwrap();

        let lib = native_library("org.lwjgl:lwjgl:3.3.1");
        unzip_native_library(&jar_path, &dest, &lib, false, false).unwrap();

        assert_eq!(
            std::fs::read(dest.join("lwjgl.dll")).unwrap(),
            b"stale content",
            "existing file must be left untouched"
        );
    }

    #[test]
    fn decompress_natives_skips_libraries_not_applicable_to_platform() {
        let dir = tmp_dir("skip_wrong_platform");
        let repo = GameRepository::new(&dir);

        let mut version = Version::new("test");
        let mut lib = native_library("org.lwjgl:lwjgl:3.3.1");
        lib.rules = vec![CompatibilityRule {
            action: RuleAction::Allow,
            os: Some(OsRestriction {
                name: Some("osx".to_string()),
                version: None,
                arch: None,
            }),
            features: None,
        }];
        version.libraries = vec![lib];

        let dest = dir.join("natives");
        decompress_natives(&version, &repo, &dest, Platform::WINDOWS_X64, false, false).unwrap();
        assert!(
            dest.exists(),
            "destination directory must still be created even with nothing to extract"
        );
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn pump_lines_forwards_each_line_from_the_stream() {
        let data = b"first line\nsecond line\nthird\n".to_vec();
        let cursor = std::io::Cursor::new(data);
        let mut collected = Vec::new();
        pump_lines(cursor, |line| collected.push(line))
            .await
            .unwrap();
        assert_eq!(
            collected,
            vec![
                "first line".to_string(),
                "second line".to_string(),
                "third".to_string()
            ]
        );
    }

    fn test_java() -> JavaRuntime {
        JavaRuntime {
            binary: PathBuf::from(r"C:\fake\java.exe"),
            info: JavaInfo::new(
                Platform {
                    os: OperatingSystem::Windows,
                    arch: Architecture::X86_64,
                },
                "0",
                None,
            ),
            is_managed: false,
            is_jdk: false,
        }
    }

    #[tokio::test]
    async fn launch_actually_spawns_a_real_process_and_captures_its_output() {
        let dir = tmp_dir("real_spawn");
        let repo = GameRepository::new(&dir);
        let version = Version::new("test");

        let mut cmd = CommandBuilder::new();
        cmd.add("cmd.exe");
        cmd.add_without_parsing("/C");
        cmd.add_without_parsing("echo hello-from-managed-process & echo %INST_NAME% 1>&2");
        let generated = GeneratedCommand {
            command: cmd,
            java_native_folder: dir.join("natives"),
            temp_native_folder: None,
            encoding: "UTF-8",
        };

        let mut options = LaunchOptions::new(&dir, test_java());
        options.use_custom_natives = true; // 跳过 natives 解压, 这个测试不关心它
        options.version_name = Some("MyTestInstance".to_string());

        let mut process = launch(&repo, &version, &options, generated)
            .await
            .expect("should spawn cmd.exe successfully");

        let stdout = process.child.stdout.take().unwrap();
        let stderr = process.child.stderr.take().unwrap();

        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        pump_lines(stdout, |l| stdout_lines.push(l)).await.unwrap();
        pump_lines(stderr, |l| stderr_lines.push(l)).await.unwrap();

        let status = process.wait().await.unwrap();
        assert!(status.success(), "cmd.exe should exit 0");
        assert!(
            stdout_lines
                .iter()
                .any(|l| l.contains("hello-from-managed-process")),
            "stdout: {stdout_lines:?}"
        );
        assert!(
            stderr_lines.iter().any(|l| l.contains("MyTestInstance")),
            "INST_NAME env var must be visible to the child process; stderr: {stderr_lines:?}"
        );
    }

    #[tokio::test]
    async fn launch_a_real_jvm_if_one_is_available_on_this_machine() {
        let candidates = [
            std::env::var("JAVA_HOME")
                .ok()
                .map(|home| PathBuf::from(home).join("bin").join("java.exe")),
            Some(PathBuf::from(
                r"C:\Users\People11\AppData\Roaming\MSYS2\usr\bin\java.exe",
            )),
        ];
        let Some(java_path) = candidates.into_iter().flatten().find(|p| p.is_file()) else {
            eprintln!("skipping: no java.exe found on this machine");
            return;
        };

        let dir = tmp_dir("real_jvm");
        let repo = GameRepository::new(&dir);
        let version = Version::new("test");

        let mut cmd = CommandBuilder::new();
        cmd.add(java_path.to_string_lossy().into_owned());
        cmd.add_without_parsing("-version");
        let generated = GeneratedCommand {
            command: cmd,
            java_native_folder: dir.join("natives"),
            temp_native_folder: None,
            encoding: "UTF-8",
        };

        let mut options = LaunchOptions::new(&dir, test_java());
        options.use_custom_natives = true;

        let mut process = launch(&repo, &version, &options, generated)
            .await
            .expect("should spawn a real java.exe");
        let stderr = process.child.stderr.take().unwrap();
        let mut lines = Vec::new();
        pump_lines(stderr, |l| lines.push(l)).await.unwrap();
        let status = process.wait().await.unwrap();

        assert!(status.success(), "java -version should exit 0");
        assert!(
            lines.iter().any(|l| l.to_lowercase().contains("version")),
            "expected a version banner on stderr, got: {lines:?}"
        );
    }
}

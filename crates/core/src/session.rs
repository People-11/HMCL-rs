use std::path::{Path, PathBuf};

use crate::download::{CacheRepository, DownloadProvider};
use crate::install::{self, GameRepository, InstallError, InstallReport};
use crate::java::{find_a_java, JavaDetectError, JavaRuntime};
use crate::launch::{
    self, AuthInfo, LaunchError, LaunchOptions, ManagedProcess, ProcessLaunchError,
};
use crate::version::{Env, Version};

pub struct LaunchRequest<'a> {
    pub client: &'a reqwest::Client,
    pub provider: &'a DownloadProvider,
    pub cache: &'a CacheRepository,
    pub repo: &'a GameRepository,
    pub dir: &'a Path,
    pub env: Env<'a>,
    /// 已经 `resolve()` 过的最终版本——`--version`/`--instance` 两条路径怎么解出
    /// 这个 `Version`（要不要先落盘成实例）是调用方自己的事，这里只管装它、启动它。
    pub version: Version,
    pub auth: AuthInfo,
    pub default_max_memory: u32,
    pub default_auto_memory: bool,
    pub default_min_memory: Option<u32>,
    pub default_metaspace: Option<u32>,
    pub default_window_width: i32,
    pub default_window_height: i32,
    pub default_fullscreen: bool,
    pub default_debug_log_output: bool,
    pub default_no_jvm_options: bool,
    pub default_no_optimizing_jvm_options: bool,
    pub default_jvm_options: Option<String>,
    pub default_game_arguments: Option<String>,
    pub default_quick_play_option: Option<launch::QuickPlayOption>,
    pub quick_play_override: Option<launch::QuickPlayOption>,
    pub default_wrapper: Option<String>,
    pub default_process_priority: launch::ProcessPriority,
    pub default_graphics_backend: launch::GraphicsApi,
    pub default_environment_variables: Option<String>,
    pub default_pre_launch_command: Option<String>,
    pub default_post_exit_command: Option<String>,
    pub default_use_custom_natives: bool,
    pub default_natives_directory: Option<String>,
    pub install_only: bool,
    pub java_override: Option<PathBuf>,
}

pub enum LaunchEvent {
    InstallSummary(InstallReport),
    JavaDetected(JavaRuntime),
    CommandLine(String),
    Warning(String),
}

/// 真正跑起来的进程 + 调用方在它退出之后还要做的事。`post_exit_command` 没有在
/// `install_and_launch` 内部执行——这个函数在进程刚起来就返回了（stdout/stderr
/// 转发和 `wait()` 都是调用方自己的事，见函数文档），"游戏退出之后"这个时间点
/// 只有调用方知道，所以把命令原样交回去，调用方自己在它的 `process.wait()`
/// 之后调 [`run_user_command`]。
pub struct LaunchedProcess {
    pub process: ManagedProcess,
    pub post_exit_command: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error(transparent)]
    JavaDetect(#[from] JavaDetectError),
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Process(#[from] ProcessLaunchError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub async fn install_and_launch(
    req: LaunchRequest<'_>,
    on_event: impl FnMut(LaunchEvent),
) -> Result<Option<LaunchedProcess>, SessionError> {
    prepare_and_maybe_launch(req, None, on_event).await
}

pub async fn generate_launch_script(
    req: LaunchRequest<'_>,
    output: &Path,
    on_event: impl FnMut(LaunchEvent),
) -> Result<(), SessionError> {
    prepare_and_maybe_launch(req, Some(output), on_event).await?;
    Ok(())
}

async fn prepare_and_maybe_launch(
    req: LaunchRequest<'_>,
    script_output: Option<&Path>,
    mut on_event: impl FnMut(LaunchEvent),
) -> Result<Option<LaunchedProcess>, SessionError> {
    let LaunchRequest {
        client,
        provider,
        cache,
        repo,
        dir,
        env,
        version,
        auth,
        default_max_memory,
        default_auto_memory,
        default_min_memory,
        default_metaspace,
        default_window_width,
        default_window_height,
        default_fullscreen,
        default_debug_log_output,
        default_no_jvm_options,
        default_no_optimizing_jvm_options,
        default_jvm_options,
        default_game_arguments,
        default_quick_play_option,
        quick_play_override,
        default_wrapper,
        default_process_priority,
        default_graphics_backend,
        default_environment_variables,
        default_pre_launch_command,
        default_post_exit_command,
        default_use_custom_natives,
        default_natives_directory,
        install_only,
        java_override,
    } = req;

    let report = install::install_version(client, provider, cache, repo, &version, env).await?;
    on_event(LaunchEvent::InstallSummary(report));

    if install_only {
        return Ok(None);
    }

    let java = find_a_java(java_override.as_deref())?;
    on_event(LaunchEvent::JavaDetected(java.clone()));

    let mut options = LaunchOptions::new(dir, java);

    // 实例自己的 instance-game-settings.json 有覆盖的话必须生效，不然设置页写了
    // 也是白写。没有覆盖就照旧用调用方给的默认值/硬编码默认值。
    let instance_settings = crate::settings::instance_game_settings::load(repo, &version.id);

    use crate::settings::instance_game_settings::*;
    let no_jvm_options = if instance_settings.is_overridden(PROPERTY_NO_JVM_OPTIONS) {
        instance_settings.effective_no_jvm_options()
    } else {
        default_no_jvm_options
    };
    options.no_generated_jvm_args = no_jvm_options;
    options.no_generated_optimizing_jvm_args = no_jvm_options
        || if instance_settings.is_overridden(PROPERTY_NO_OPTIMIZING_JVM_OPTIONS) {
            instance_settings.effective_no_optimizing_jvm_options()
        } else {
            default_no_optimizing_jvm_options
        };
    if no_jvm_options {
        options.max_memory = None;
        options.min_memory = None;
        options.metaspace = None;
    } else {
        options.max_memory = Some(
            if instance_settings.effective_auto_memory(default_auto_memory) {
                crate::settings::instance_game_settings::auto_allocated_memory_mb()
            } else {
                instance_settings.effective_max_memory(default_max_memory)
            },
        );
        options.min_memory = if instance_settings.is_overridden(PROPERTY_MIN_MEMORY) {
            instance_settings.effective_min_memory()
        } else {
            default_min_memory
        };
        options.metaspace = if instance_settings.is_overridden(PROPERTY_PERM_SIZE) {
            instance_settings.effective_metaspace()
        } else {
            default_metaspace
        };
    }
    let jvm_options = if instance_settings.is_overridden(PROPERTY_JVM_OPTIONS) {
        instance_settings.effective_jvm_options()
    } else {
        default_jvm_options
            .as_deref()
            .filter(|value| !value.is_empty())
    };
    if let Some(extra) = jvm_options {
        options.java_arguments.extend(launch::tokenize(extra));
    }

    let (width, height, mut fullscreen) =
        instance_settings.effective_window(default_window_width, default_window_height);
    if !instance_settings.is_overridden(PROPERTY_WINDOW_TYPE) {
        fullscreen = default_fullscreen;
    }
    options.width = width;
    options.height = height;
    options.fullscreen = fullscreen;

    options.quick_play_option = if quick_play_override.is_some() {
        quick_play_override
    } else if instance_settings.is_overridden(PROPERTY_QUICK_PLAY) {
        instance_settings.effective_quick_play_option()
    } else {
        default_quick_play_option
    };
    let game_arguments = if instance_settings.is_overridden(PROPERTY_GAME_ARGUMENTS) {
        instance_settings.effective_game_arguments()
    } else {
        default_game_arguments
            .as_deref()
            .filter(|value| !value.is_empty())
    };
    if let Some(extra) = game_arguments {
        options.game_arguments.extend(launch::tokenize(extra));
    }
    options.wrapper = if instance_settings.is_overridden(PROPERTY_COMMAND_WRAPPER) {
        instance_settings.effective_wrapper().map(str::to_string)
    } else {
        default_wrapper
    };
    options.process_priority = if instance_settings.is_overridden(PROPERTY_PROCESS_PRIORITY) {
        instance_settings.effective_process_priority()
    } else {
        default_process_priority
    };
    options.graphics_backend = if instance_settings.is_overridden(PROPERTY_GRAPHICS_BACKEND) {
        instance_settings.effective_graphics_backend()
    } else {
        default_graphics_backend
    };
    options.enable_debug_log_output = if instance_settings
        .is_overridden(crate::settings::instance_game_settings::PROPERTY_ENABLE_DEBUG_LOG_OUTPUT)
    {
        instance_settings.effective_debug_log_output()
    } else {
        default_debug_log_output
    };
    options.use_custom_natives = if instance_settings.is_overridden(PROPERTY_USE_CUSTOM_NATIVES) {
        instance_settings.effective_use_custom_natives()
    } else {
        default_use_custom_natives
    };
    options.natives_dir = if instance_settings.is_overridden(PROPERTY_NATIVES_DIRECTORY) {
        instance_settings
            .effective_natives_directory()
            .map(str::to_string)
    } else {
        default_natives_directory.filter(|value| !value.is_empty())
    };
    options.extra_environment_variables =
        if instance_settings.is_overridden(PROPERTY_ENVIRONMENT_VARIABLES) {
            instance_settings.effective_environment_variables()
        } else {
            default_environment_variables
                .as_deref()
                .map(parse_environment_variables)
                .unwrap_or_default()
        };

    let pre_launch_command = if instance_settings.is_overridden(PROPERTY_PRE_LAUNCH_COMMAND) {
        instance_settings
            .effective_pre_launch_command()
            .map(str::to_string)
    } else {
        default_pre_launch_command.filter(|value| !value.is_empty())
    };
    if script_output.is_none() {
        if let Some(pre) = pre_launch_command.as_deref() {
            if let Err(e) = run_user_command(pre).await {
                on_event(LaunchEvent::Warning(format!(
                    "preLaunchCommand 执行失败(已忽略, 继续启动): {e}"
                )));
            }
        }
    }
    let post_exit_command = if instance_settings.is_overridden(PROPERTY_POST_EXIT_COMMAND) {
        instance_settings
            .effective_post_exit_command()
            .map(str::to_string)
    } else {
        default_post_exit_command.filter(|value| !value.is_empty())
    };

    let native_folder = repo.native_directory(&version.id, env.platform);
    let generated =
        launch::generate_command_line(repo, &version, &auth, &options, &native_folder, env)?;
    on_event(LaunchEvent::CommandLine(generated.command.render()));

    if let Some(output) = script_output {
        write_batch_script(
            output,
            &repo.run_directory(&version.id),
            pre_launch_command.as_deref(),
            &generated.command.render(),
            post_exit_command.as_deref(),
        )?;
        return Ok(None);
    }

    let process = launch::launch(repo, &version, &options, generated).await?;
    Ok(Some(LaunchedProcess {
        process,
        post_exit_command,
    }))
}

fn write_batch_script(
    output: &Path,
    run_directory: &Path,
    pre_launch_command: Option<&str>,
    command_line: &str,
    post_exit_command: Option<&str>,
) -> std::io::Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lines = vec![
        "@echo off".to_string(),
        format!(
            "cd /d {}",
            launch::command_builder::to_batch_string_literal(&run_directory.to_string_lossy())
        ),
    ];
    if let Some(command) = pre_launch_command {
        lines.push(command.to_string());
    }
    lines.push(command_line.to_string());
    if let Some(command) = post_exit_command {
        lines.push(command.to_string());
    }
    lines.push("pause".to_string());
    std::fs::write(output, lines.join("\r\n") + "\r\n")
}

/// 执行 `preLaunchCommand`/`postExitCommand` 这种用户自己敲的一整条命令行——跟
/// 真正的游戏命令行一样用 [`launch::tokenize`] 切分（第一个 token 是可执行文件,
/// 其余是参数）。调用方决定失败了要不要紧（`install_and_launch` 对
/// `preLaunchCommand` 的态度是"记一条警告, 继续启动"）。
pub async fn run_user_command(command_line: &str) -> std::io::Result<()> {
    let tokens = launch::tokenize(command_line);
    let Some((program, args)) = tokens.split_first() else {
        return Ok(());
    };
    let mut command = tokio::process::Command::new(program);
    crate::platform::hide_console_window(&mut command);
    let status = command.args(args).status().await?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "command exited with {status}"
        )));
    }
    Ok(())
}

fn parse_environment_variables(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            (!key.is_empty()).then(|| (key.to_string(), value.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_script_keeps_working_directory_and_user_commands() {
        let root = std::env::temp_dir()
            .join("hmcl-rs-test")
            .join("launch_script");
        let output = root.join("launch.bat");
        write_batch_script(
            &output,
            Path::new(r"C:\Games\Minecraft Test"),
            Some("echo before"),
            r#""C:\Java\java.exe" -version"#,
            Some("echo after"),
        )
        .unwrap();
        let text = std::fs::read_to_string(output).unwrap();
        assert!(text.contains(r#"cd /d "C:\\Games\\Minecraft Test""#));
        assert!(text.contains("echo before\r\n\"C:\\Java\\java.exe\" -version\r\necho after"));
    }
}

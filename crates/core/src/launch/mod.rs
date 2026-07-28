use std::collections::HashMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::install::GameRepository;
use crate::java::JavaRuntime;
use crate::platform::Bits;
use crate::version::{Argument, Arguments, Env, Version};
use crate::versioning::GameVersionNumber;

pub mod command_builder;
pub mod process;
pub use command_builder::CommandBuilder;
pub use process::{
    decompress_natives, launch, pump_lines, ManagedProcess, NativesError, ProcessLaunchError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress {
    pub host: String,
    pub port: Option<u16>,
}

impl ServerAddress {
    pub fn parse(address: &str) -> Result<ServerAddress, String> {
        let invalid = || format!("Invalid server address: {address}");

        if !address.starts_with('[') {
            match address.find(':') {
                Some(colon) => {
                    if colon == address.len() - 1 {
                        return Err(invalid());
                    }
                    let host = address[..colon].to_string();
                    let port: u16 = address[colon + 1..].parse().map_err(|_| invalid())?;
                    Ok(ServerAddress {
                        host,
                        port: Some(port),
                    })
                }
                None => Ok(ServerAddress {
                    host: address.to_string(),
                    port: None,
                }),
            }
        } else {
            let colon = address.find(':').ok_or_else(invalid)?;
            let close = address.rfind(']').ok_or_else(invalid)?;
            if close < colon {
                return Err(invalid());
            }
            let host = address[1..close].to_string();
            if close == address.len() - 1 {
                return Ok(ServerAddress { host, port: None });
            }
            if address.len() < close + 3 || address.as_bytes()[close + 1] != b':' {
                return Err(invalid());
            }
            let port: u16 = address[close + 2..].parse().map_err(|_| invalid())?;
            Ok(ServerAddress {
                host,
                port: Some(port),
            })
        }
    }
}

/// 对应 Java `StringUtils.tokenize(String)`（不带 `vars` 的单参数重载）：按空格分词，
/// 单引号内容原样保留，双引号内容支持 `` `n``/`` `t`` 这几个 HMCL 自定义的反引号
/// 转义（不是标准 shell 的反斜杠转义）。
///
/// ponytail: 没做 `$VAR`/`%VAR%` 环境变量替换——那只在 `wrapper` 字段非空时才有意义
/// （用 `getEnvVars()` 提供的一批 `INST_*` 变量），而 `wrapper` 是给 Linux/macOS 上
/// `optirun`/`prime-run` 这类 GPU 切换脚本用的，Windows 用户基本不会设置它。
/// version.json 里的旧式 `minecraftArguments` 字段（本函数的另一个调用方）从来
/// 不含 `$`，不受影响。
pub fn tokenize(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut has_value = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            has_value = true;
            let end = chars[i + 1..]
                .iter()
                .position(|&ch| ch == '\'')
                .map(|p| i + 1 + p)
                .unwrap_or(chars.len());
            current.extend(&chars[i + 1..end]);
            i = end + 1;
        } else if c == '"' {
            has_value = true;
            i += 1;
            while i < chars.len() {
                let ch = chars[i];
                i += 1;
                if ch == '"' {
                    break;
                } else if ch == '`' && i < chars.len() {
                    let esc = chars[i];
                    i += 1;
                    current.push(match esc {
                        'a' => '\u{07}',
                        'b' => '\u{08}',
                        'f' => '\u{0C}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'v' => '\u{0B}',
                        other => other,
                    });
                } else {
                    current.push(ch);
                }
            }
        } else if c == ' ' {
            if has_value {
                parts.push(std::mem::take(&mut current));
                has_value = false;
            }
            i += 1;
        } else {
            has_value = true;
            current.push(c);
            i += 1;
        }
    }
    if has_value {
        parts.push(current);
    }
    parts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessPriority {
    Low,
    BelowNormal,
    #[default]
    Normal,
    AboveNormal,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GraphicsApi {
    #[default]
    Default,
    #[serde(rename = "OPENGL")]
    OpenGl,
    Vulkan,
}

impl GraphicsApi {
    pub fn minecraft_arg(self) -> &'static str {
        match self {
            GraphicsApi::Default => "default",
            GraphicsApi::OpenGl => "opengl",
            GraphicsApi::Vulkan => "vulkan",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Renderer {
    #[default]
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickPlayOption {
    SinglePlayer { world_folder_name: String },
    MultiPlayer { server_ip: String },
    Realm { realm_id: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyOption {
    Direct,
    #[default]
    Default,
    Http {
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
    },
    Socks {
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct AuthInfo {
    pub username: String,
    pub uuid: Uuid,
    pub access_token: String,
    pub user_type: String,
    pub user_properties: String,
    pub launch_arguments: Option<Arguments>,
}

pub const USER_TYPE_MSA: &str = "msa";
pub const USER_TYPE_MOJANG: &str = "mojang";
pub const USER_TYPE_LEGACY: &str = "legacy";

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub game_dir: PathBuf,
    pub java: JavaRuntime,
    pub version_name: Option<String>,
    pub version_type: Option<String>,
    pub profile_name: Option<String>,
    pub game_arguments: Vec<String>,
    pub override_java_arguments: Vec<String>,
    pub java_arguments: Vec<String>,
    pub java_agents: Vec<String>,
    pub min_memory: Option<u32>,
    pub max_memory: Option<u32>,
    pub metaspace: Option<u32>,
    pub width: i32,
    pub height: i32,
    pub fullscreen: bool,
    pub quick_play_option: Option<QuickPlayOption>,
    pub wrapper: Option<String>,
    pub proxy_option: ProxyOption,
    pub no_generated_jvm_args: bool,
    pub no_generated_optimizing_jvm_args: bool,
    pub process_priority: ProcessPriority,
    pub graphics_backend: GraphicsApi,
    pub renderer: Renderer,
    pub enable_debug_log_output: bool,
    pub use_custom_natives: bool,
    pub natives_dir: Option<String>,
    pub use_native_glfw: bool,
    pub use_native_openal: bool,
    /// 对应 Java `GameSettings.environmentVariables`：用户自己加的额外环境变量，
    /// 在 `INST_*`/`APPDATA` 这几个固定变量之后设置，所以用户能覆盖它们
    /// （虽然没什么理由这么做，但跟 Java 版行为一致，不额外加限制）。
    pub extra_environment_variables: Vec<(String, String)>,
}

impl LaunchOptions {
    pub fn new(game_dir: impl Into<PathBuf>, java: JavaRuntime) -> LaunchOptions {
        LaunchOptions {
            game_dir: game_dir.into(),
            java,
            version_name: None,
            version_type: None,
            profile_name: None,
            game_arguments: Vec::new(),
            override_java_arguments: Vec::new(),
            java_arguments: Vec::new(),
            java_agents: Vec::new(),
            min_memory: None,
            max_memory: None,
            metaspace: None,
            width: 1280,
            height: 720,
            fullscreen: false,
            quick_play_option: None,
            wrapper: None,
            proxy_option: ProxyOption::default(),
            no_generated_jvm_args: false,
            no_generated_optimizing_jvm_args: false,
            process_priority: ProcessPriority::default(),
            graphics_backend: GraphicsApi::default(),
            renderer: Renderer::default(),
            enable_debug_log_output: false,
            use_custom_natives: false,
            natives_dir: None,
            use_native_glfw: false,
            use_native_openal: false,
            extra_environment_variables: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("Minecraft jar does not exist: {0}")]
    JarMissing(PathBuf),
    #[error("main class is null for instance {0}")]
    NoMainClass(String),
}

#[derive(Debug)]
pub struct GeneratedCommand {
    pub command: CommandBuilder,
    pub java_native_folder: PathBuf,
    /// 只有非 Windows、natives 目录路径非 ASCII 且游戏版本 < 1.19 时才会是 `Some`
    /// （lwjgl 早期版本假设 natives 路径是 ASCII，这是绕过它的临时目录）。
    /// windows-gnu 上恒为 `None`——见模块顶部 ponytail 注释。
    pub temp_native_folder: Option<PathBuf>,
    pub encoding: &'static str,
}

/// ponytail: 命令行生成只需要一个字符串塞进 `-Dfile.encoding=`，UTF-8 对
/// Java 18+ 本来就是默认值，对更老的 Java 只在极端情况下（老版本 Java + 非英文
/// Windows + 游戏自己往 stdout 打非 ASCII 字符）才会有影响。
fn native_charset_name() -> &'static str {
    "UTF-8"
}

fn total_memory_bytes() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory()
}

/// 对应 Java `DefaultLauncher.isUsingLog4j`。gameVersion 解析不了（老 alpha/beta/
/// 周快照格式）时保守当作"没用 log4j"——这些老版本本来就没有 log4j，不是瞎猜。
fn is_using_log4j(game_version: Option<&str>) -> bool {
    match game_version {
        None => true, // 对应 Java `orElse("1.7")`, compare("1.7","1.7") = Equal
        Some(v) => matches!(
            GameVersionNumber::compare(v, "1.7"),
            Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
        ),
    }
}

// 对应 Java `HMCLCore/src/main/resources/assets/game/log4j2-*.xml`：Mojang 官方
// 标准的 log4j2 配置模板（不是 HMCL 自己发明的格式，官方启动器和其它第三方launcher
// 用的都是同一套），按"游戏版本 < 1.12"和"是否开调试日志"两个维度选四选一。
const LOG4J_1_7: &str = include_str!("../../assets/log4j2-1.7.xml");
const LOG4J_1_7_DEBUG: &str = include_str!("../../assets/log4j2-1.7-debug.xml");
const LOG4J_1_12: &str = include_str!("../../assets/log4j2-1.12.xml");
const LOG4J_1_12_DEBUG: &str = include_str!("../../assets/log4j2-1.12-debug.xml");

/// 对应 Java `DefaultLauncher.extractLog4jConfigurationFile`。
///
/// 注意: 判断"是不是老版本"的默认值跟 [`is_using_log4j`] **不一样**——版本号
/// 未知/解析不了时这里选老版本模板（对应 Java `GameVersionNumber.unknown()` =
/// `Release.ZERO`，天然小于 "1.12"），而 `is_using_log4j` 未知时默认当新版本。
/// 两处照抄各自的 Java 判定源头，不是不一致，是分别对不同问题给出的各自正确答案。
fn extract_log4j_configuration_file(
    target: &std::path::Path,
    game_version: Option<&str>,
    debug: bool,
) -> std::io::Result<()> {
    let is_old = !matches!(
        GameVersionNumber::compare(game_version.unwrap_or(""), "1.12"),
        Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
    );
    let text = match (is_old, debug) {
        (true, false) => LOG4J_1_7,
        (true, true) => LOG4J_1_7_DEBUG,
        (false, false) => LOG4J_1_12,
        (false, true) => LOG4J_1_12_DEBUG,
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, text)
}

/// 对应 Java `DefaultLauncher.forbiddens`：`-Xincgc` 在 Java 9+ 已经被移除，
/// 生成出来的命令行里如果混进这个参数（比如用户的旧配置残留）会导致 JVM 直接
/// 启动失败，必须无条件去掉。
fn is_forbidden(arg: &str, java_parsed_version: Option<u32>) -> bool {
    arg == "-Xincgc" && java_parsed_version.unwrap_or(0) >= 9
}

/// 对应 Java `Version.getArguments().map(Arguments::jvm)` 里那段"找
/// `-Djava.library.path=${natives_directory}/xxx` 并计算实际 natives 子目录"的逻辑。
/// 只处理路径确实落在 `native_folder` 内部的情况（防止 `../../` 之类的路径逃逸）。
fn resolve_java_native_folder(native_folder: &Path, jvm_arguments: Option<&[Argument]>) -> PathBuf {
    const PREFIX: &str = "-Djava.library.path=${natives_directory}/";

    if let Some(args) = jvm_arguments {
        for arg in args {
            if let Argument::Plain(s) = arg {
                if let Some(sub_dir) = s.strip_prefix(PREFIX) {
                    let candidate = native_folder.join(sub_dir);
                    // 必须真的落在 native_folder 内部, 不接受把 natives 解到别的地方去。
                    if candidate.starts_with(native_folder) {
                        return candidate;
                    }
                }
                if s.starts_with("-Djava.library.path=") {
                    break;
                }
            }
        }
    }

    native_folder.to_path_buf()
}

fn base_configuration(
    repo: &GameRepository,
    version: &Version,
    options: &LaunchOptions,
    auth: &AuthInfo,
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_string(), v);
    };

    put("${auth_player_name}", auth.username.clone());
    put("${auth_session}", auth.access_token.clone());
    put("${auth_access_token}", auth.access_token.clone());
    put("${auth_uuid}", auth.uuid.simple().to_string());
    put(
        "${version_name}",
        options
            .version_name
            .clone()
            .unwrap_or_else(|| version.id.clone()),
    );
    put(
        "${profile_name}",
        options
            .profile_name
            .clone()
            .unwrap_or_else(|| "Minecraft".to_string()),
    );
    put(
        "${version_type}",
        options.version_type.clone().unwrap_or_else(|| {
            version
                .release_type
                .unwrap_or(crate::version::ReleaseType::Unknown)
                .id()
                .to_string()
        }),
    );
    put(
        "${game_directory}",
        repo.run_directory(&version.id)
            .to_string_lossy()
            .into_owned(),
    );
    put("${user_type}", auth.user_type.clone());
    put("${assets_index_name}", version.asset_index().base.id);
    put("${user_properties}", auth.user_properties.clone());
    put("${resolution_width}", options.width.to_string());
    put("${resolution_height}", options.height.to_string());
    put("${launcher_name}", "HMCL-rs".to_string());
    put("${launcher_version}", env!("CARGO_PKG_VERSION").to_string());
    put(
        "${library_directory}",
        repo.libraries_dir().to_string_lossy().into_owned(),
    );
    put("${classpath_separator}", classpath_separator().to_string());
    put(
        "${primary_jar}",
        repo.version_jar(&version.id).to_string_lossy().into_owned(),
    );
    put("${language}", "en-us".to_string());

    put(
        "${libraries_directory}",
        repo.libraries_dir().to_string_lossy().into_owned(),
    );
    put("${file_separator}", std::path::MAIN_SEPARATOR.to_string());
    put(
        "${primary_jar_name}",
        repo.version_jar(&version.id)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );

    m
}

#[cfg(windows)]
fn classpath_separator() -> char {
    ';'
}
#[cfg(not(windows))]
fn classpath_separator() -> char {
    ':'
}

/// 对应 Java `DefaultLauncher.generateCommandLine`。`version` 必须是已经
/// `resolve()` 过的（inheritsFrom 链和 patches 都已经展开）。
pub fn generate_command_line(
    repo: &GameRepository,
    version: &Version,
    auth: &AuthInfo,
    options: &LaunchOptions,
    native_folder: &Path,
    env: Env,
) -> Result<GeneratedCommand, LaunchError> {
    let mut res = CommandBuilder::new();

    // Windows 的进程优先级由进程启动层设置，不是 JVM 参数。

    if let Some(wrapper) = options.wrapper.as_deref().filter(|w| !w.trim().is_empty()) {
        res.add_all_without_parsing(tokenize(wrapper));
    }

    res.add(options.java.binary.to_string_lossy().into_owned());

    res.add_all_without_parsing_and_read_external(options.override_java_arguments.clone());

    if let Some(max) = options.max_memory.filter(|&m| m > 0) {
        res.add_default("-Xmx", &format!("{max}m"));
    }
    if let Some(min) = options
        .min_memory
        .filter(|&m| m > 0 && options.max_memory.is_none_or(|max| m <= max))
    {
        res.add_default("-Xms", &format!("{min}m"));
    }

    let java_version = options.java.parsed_version();
    if let Some(metaspace) = options.metaspace.filter(|&m| m > 0) {
        if java_version.unwrap_or(0) < 8 {
            res.add_default("-XX:PermSize=", &format!("{metaspace}m"));
        } else {
            res.add_default("-XX:MetaspaceSize=", &format!("{metaspace}m"));
        }
    }

    res.add_all_default_without_parsing(&options.java_arguments);

    let mut encoding = native_charset_name();
    let file_encoding = res.add_default("-Dfile.encoding=", encoding);
    if let Some(fe) = &file_encoding {
        if fe != "-Dfile.encoding=COMPAT" {
            encoding = "UTF-8"; // 见 native_charset_name 的 ponytail 说明: 现阶段只有 UTF-8 一种可能
        }
    }

    if java_version.unwrap_or(0) < 19 {
        res.add_default("-Dsun.stdout.encoding=", encoding);
        res.add_default("-Dsun.stderr.encoding=", encoding);
    } else {
        res.add_default("-Dstdout.encoding=", encoding);
        res.add_default("-Dstderr.encoding=", encoding);
    }

    res.add_default("-Djava.rmi.server.useCodebaseOnly=", "true");
    res.add_default("-Dcom.sun.jndi.rmi.object.trustURLCodebase=", "false");
    res.add_default("-Dcom.sun.jndi.cosnaming.object.trustURLCodebase=", "false");

    // ponytail: Java 版这里是扫描 client.jar 的 class 常量池挖出真实游戏版本
    // ——独立于版本 json 的 id, 专门用来兼容"文件夹改了名字/
    // Forge 整合包 id 是 '1.20.1-forge-47.2.0' 这种不直接是版本号"的情况。
    // 那部分常量池解析没做, 先直接拿 version.id 当近似值: 对纯净版(id 本身就是版本号,
    // 比如 "1.20.1") 完全准确; 对 Forge/Fabric 这类 id 带后缀的实例, 下面几处版本号
    // 比较会解析失败返回 None, 相应的特殊处理(log4j 配置/quickplay/graphicsBackend)
    // 会被保守跳过，退化到旧式但仍可运行的参数形式。
    let game_version: Option<String> = Some(version.id.clone());

    let format_msg_no_lookups = res.add_default("-Dlog4j2.formatMsgNoLookups=", "true");
    if is_using_log4j(game_version.as_deref())
        && (options.enable_debug_log_output
            || format_msg_no_lookups.as_deref() != Some("-Dlog4j2.formatMsgNoLookups=false"))
    {
        let log4j_config = repo.version_root(&version.id).join("log4j2.xml");
        // 这一步之前漏做了: 只生成了指向这个文件的参数, 从没真的把文件写出去,
        // 导致每次启动 log4j 都在 StatusLogger 里报 FileNotFoundException——
        // 不是致命错误(log4j 会退化到内置默认配置), 但完全没必要, 而且丢了
        // CVE-2021-44228 缓解模板本该带的 "logs/latest.log" 滚动文件功能。
        // 在真实跑通 LegacyFabric 装老版本(1.12.2)时,通过对照"纯原版 1.12.2 是否
        // 也报同样的 FileNotFoundException"发现的——两边现象一致,证明是这里漏做,
        // 不是某个加载器安装器的问题。
        // 照抄 Java 决定用哪个模板的判定源头(独立于 is_using_log4j 的判定,
        // 两者对"版本号未知时怎么办"的默认选择本来就不一样, 都各自照抄各自的源头,
        // 不强行统一)。 写失败(权限/磁盘问题)时选择降级——跳过这个参数, 让 log4j
        // 退回内置默认配置, 而不是像 Java 版那样让整个启动直接失败, 因为这只是个
        // 日志格式文件, 不是游戏能不能跑起来的必要条件。
        match extract_log4j_configuration_file(
            &log4j_config,
            game_version.as_deref(),
            options.enable_debug_log_output,
        ) {
            Ok(()) => {
                res.add_default(
                    "-Dlog4j.configurationFile=",
                    &log4j_config.to_string_lossy(),
                );
            }
            Err(e) => {
                tracing::warn!(path = %log4j_config.display(), error = %e, "failed to write log4j2.xml, launching without a custom log4j config")
            }
        }
    }

    if !options.no_generated_jvm_args {
        res.add_default(
            "-Dminecraft.client.jar=",
            &repo.version_jar(&version.id).to_string_lossy(),
        );

        // (跳过: macOS -Xdock:name/-Xdock:icon; 非 Windows 的 -Duser.home 重定向 —— 见模块顶部说明)

        let has_proxy_flag = res.none_match(|a| {
            a.starts_with("-Djava.net.useSystemProxies=")
                || a.starts_with("-Dhttp.proxy")
                || a.starts_with("-Dhttps.proxy")
                || a.starts_with("-DsocksProxy")
                || a.starts_with("-Djava.net.socks.")
        });
        if has_proxy_flag {
            match &options.proxy_option {
                ProxyOption::Direct => {}
                ProxyOption::Default => {
                    res.add("-Djava.net.useSystemProxies=true");
                }
                ProxyOption::Http {
                    host,
                    port,
                    username,
                    password,
                } => {
                    res.add(format!("-Dhttp.proxyHost={host}"));
                    res.add(format!("-Dhttp.proxyPort={port}"));
                    res.add(format!("-Dhttps.proxyHost={host}"));
                    res.add(format!("-Dhttps.proxyPort={port}"));
                    if let Some(user) = username.as_deref().filter(|u| !u.trim().is_empty()) {
                        res.add(format!("-Dhttp.proxyUser={user}"));
                        res.add(format!(
                            "-Dhttp.proxyPassword={}",
                            password.clone().unwrap_or_default()
                        ));
                        res.add(format!("-Dhttps.proxyUser={user}"));
                        res.add(format!(
                            "-Dhttps.proxyPassword={}",
                            password.clone().unwrap_or_default()
                        ));
                    }
                }
                ProxyOption::Socks {
                    host,
                    port,
                    username,
                    password,
                } => {
                    res.add(format!("-DsocksProxyHost={host}"));
                    res.add(format!("-DsocksProxyPort={port}"));
                    if let Some(user) = username.as_deref().filter(|u| !u.trim().is_empty()) {
                        res.add(format!("-Djava.net.socks.username={user}"));
                        res.add(format!(
                            "-Djava.net.socks.password={}",
                            password.clone().unwrap_or_default()
                        ));
                    }
                }
            }
        }

        let is64bit = options.java.bits() == Bits::Bit64;
        let jv = java_version.unwrap_or(0);

        if !options.no_generated_optimizing_jvm_args {
            res.add_unstable_default("UnlockExperimentalVMOptions", true);
            res.add_unstable_default("UnlockDiagnosticVMOptions", true);

            if jv >= 8
                && res.none_match(|a| {
                    a == "-XX:-UseG1GC" || (a.starts_with("-XX:+Use") && a.ends_with("GC"))
                })
            {
                res.add_unstable_default("UseG1GC", true);
                res.add_unstable_default_kv("G1MixedGCCountTarget", "5");
                res.add_unstable_default_kv("G1NewSizePercent", "20");
                res.add_unstable_default_kv("G1ReservePercent", "20");
                res.add_unstable_default_kv("MaxGCPauseMillis", "50");
                res.add_unstable_default_kv("G1HeapRegionSize", "32m");
            }

            res.add_unstable_default("OmitStackTraceInFastThrow", false);

            if jv <= 8 {
                res.add_unstable_default_kv("MaxInlineLevel", "15");
            }
            if is64bit && total_memory_bytes() > 4 * 1024 * 1024 * 1024 {
                res.add_unstable_default("DontCompileHugeMethods", false);
                res.add_unstable_default_kv("MaxNodeLimit", "240000");
                res.add_unstable_default_kv("NodeLimitFudgeFactor", "8000");
                res.add_unstable_default_kv("TieredCompileTaskTimeout", "10000");
                res.add_unstable_default_kv("ReservedCodeCacheSize", "400M");
                if jv >= 9 {
                    res.add_unstable_default_kv("NonNMethodCodeHeapSize", "12M");
                    res.add_unstable_default_kv("ProfiledCodeHeapSize", "194M");
                }
                if jv >= 8 {
                    res.add_unstable_default_kv("NmethodSweepActivity", "1");
                }
            }

            if is64bit && (25..=26).contains(&jv) {
                res.add_unstable_default("UseCompactObjectHeaders", true);
            }

            if !is64bit {
                res.add_default("-Xss", "1m");
            }
        }

        if jv == 16 {
            res.add_default("--illegal-access=", "permit");
        }
        if jv == 24 || jv == 25 {
            res.add_default("--sun-misc-unsafe-memory-access=", "allow");
        }

        res.add_default("-Dfml.ignoreInvalidMinecraftCertificates=", "true");
        res.add_default("-Dfml.ignorePatchDiscrepancies=", "true");
    }

    let mut classpath = repo.classpath(version, env);
    let jar = repo.version_jar(&version.id);
    if !jar.is_file() {
        return Err(LaunchError::JarMissing(jar));
    }
    classpath.push(jar.to_string_lossy().into_owned());

    let asset_index = version.asset_index();
    let game_assets = repo.actual_asset_directory(&asset_index.base.id, false); // (虚拟 assets 目录物化没做, 见 install.rs 的 ponytail 说明)

    let mut configuration = base_configuration(repo, version, options, auth);
    configuration.insert(
        "${classpath}".to_string(),
        classpath.join(&classpath_separator().to_string()),
    );
    configuration.insert(
        "${game_assets}".to_string(),
        game_assets.to_string_lossy().into_owned(),
    );
    configuration.insert(
        "${assets_root}".to_string(),
        game_assets.to_string_lossy().into_owned(),
    );

    let native_folder_path = native_folder.to_string_lossy().into_owned();
    let temp_native_folder: Option<PathBuf> = None;
    configuration.insert("${natives_directory}".to_string(), native_folder_path);

    let jvm_arguments: Option<&[Argument]> =
        version.arguments.as_ref().and_then(|a| a.jvm.as_deref());
    let java_native_folder = resolve_java_native_folder(native_folder, jvm_arguments);

    let default_jvm_args = crate::version::default_jvm_arguments();
    let jvm_args_to_use: &[Argument] = jvm_arguments.unwrap_or(&default_jvm_args);
    res.add_all(Arguments::parse_arguments(
        jvm_args_to_use,
        &configuration,
        env,
        &HashMap::new(),
    ));

    if let Some(auth_args) = &auth.launch_arguments {
        if let Some(jvm) = auth_args.jvm.as_ref().filter(|v| !v.is_empty()) {
            res.add_all(Arguments::parse_arguments(
                jvm,
                &configuration,
                env,
                &HashMap::new(),
            ));
        }
    }

    for agent in &options.java_agents {
        res.add(format!("-javaagent:{agent}"));
    }

    let main_class = version
        .main_class
        .clone()
        .ok_or_else(|| LaunchError::NoMainClass(version.id.clone()))?;
    res.add(main_class);

    if let Some(legacy_args) = &version.minecraft_arguments {
        let tokens = tokenize(legacy_args);
        res.add_all(Arguments::parse_string_arguments(&tokens, &configuration));
    }

    let mut features = HashMap::new();
    features.insert(
        "has_custom_resolution".to_string(),
        options.width != 0 && options.height != 0,
    );

    if let Some(game_args) = version.arguments.as_ref().and_then(|a| a.game.as_deref()) {
        res.add_all(Arguments::parse_arguments(
            game_args,
            &configuration,
            env,
            &features,
        ));
    }
    if version.minecraft_arguments.is_some() {
        res.add_all(Arguments::parse_arguments(
            &crate::version::default_game_arguments(),
            &configuration,
            env,
            &features,
        ));
    }
    if let Some(auth_args) = &auth.launch_arguments {
        if let Some(game) = auth_args.game.as_ref().filter(|v| !v.is_empty()) {
            res.add_all(Arguments::parse_arguments(
                game,
                &configuration,
                env,
                &features,
            ));
        }
    }

    if let Some(qp) = &options.quick_play_option {
        match qp {
            QuickPlayOption::MultiPlayer { server_ip } => match ServerAddress::parse(server_ip) {
                Ok(addr) => {
                    if supports_quick_play(game_version.as_deref()) {
                        res.add("--quickPlayMultiplayer");
                        res.add(match addr.port {
                            Some(_) => server_ip.clone(),
                            None => format!("{}:25565", addr.host),
                        });
                    } else {
                        res.add("--server");
                        res.add(addr.host.clone());
                        res.add("--port");
                        res.add(addr.port.unwrap_or(25565).to_string());
                    }
                }
                Err(e) => {
                    tracing::warn!(address = server_ip, error = %e, "invalid server address");
                }
            },
            QuickPlayOption::SinglePlayer { world_folder_name }
                if supports_quick_play(game_version.as_deref()) =>
            {
                res.add("--quickPlaySingleplayer");
                res.add(world_folder_name.clone());
            }
            QuickPlayOption::Realm { realm_id } if supports_quick_play(game_version.as_deref()) => {
                res.add("--quickPlayRealms");
                res.add(realm_id.clone());
            }
            _ => {}
        }
    }

    if options.fullscreen {
        res.add("--fullscreen");
    }

    if let ProxyOption::Socks {
        host,
        port,
        username,
        password,
    } = &options.proxy_option
    {
        res.add("--proxyHost");
        res.add(host.clone());
        res.add("--proxyPort");
        res.add(port.to_string());
        if let Some(user) = username.as_deref().filter(|u| !u.trim().is_empty()) {
            res.add("--proxyUser");
            res.add(user.to_string());
            res.add("--proxyPass");
            res.add(password.clone().unwrap_or_default());
        }
    }

    if options.graphics_backend != GraphicsApi::Default
        && game_version.as_deref().is_some_and(|v| {
            matches!(
                GameVersionNumber::compare(v, "26.2-snapshot-2"),
                Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
            )
        })
    {
        res.add("--graphicsBackend");
        res.add(options.graphics_backend.minecraft_arg());
    }

    res.add_all_without_parsing(Arguments::parse_string_arguments(
        &options.game_arguments,
        &configuration,
    ));

    let jv_for_forbidden = java_version;
    res.remove_if(|a| is_forbidden(a, jv_for_forbidden));

    Ok(GeneratedCommand {
        command: res,
        java_native_folder,
        temp_native_folder,
        encoding,
    })
}

/// 对应 Java `World.supportQuickPlay`：1.20 正式版（含）以后支持新版 quickplay 参数
/// （`--quickPlaySingleplayer` 等），更早的版本要用 `--server`/`--port`。
///
/// ponytail: Java 原版还认 `"23w14a"` 这个 1.20 之前的周快照（`isAtLeast("1.20",
/// "23w14a")`），我们的 `GameVersionNumber` 不解析周快照格式，所以这里退化成只按
/// 正式版 1.20 判断——1.20 之前的某几周快照会因此被误判成"不支持"，走旧式
/// `--server`/`--port` 参数，游戏照样能连上服务器，只是少了 quickplay 的直接进服
/// 体验。真正需要支持这些快照时去 `versioning.rs` 补 `LegacySnapshot` 分支。
fn supports_quick_play(game_version: Option<&str>) -> bool {
    game_version.is_some_and(|v| {
        matches!(
            GameVersionNumber::compare(v, "1.20"),
            Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_address_parses_host_port_and_bare_host() {
        assert_eq!(
            ServerAddress::parse("mc.example.com:25566").unwrap(),
            ServerAddress {
                host: "mc.example.com".to_string(),
                port: Some(25566)
            }
        );
        assert_eq!(
            ServerAddress::parse("mc.example.com").unwrap(),
            ServerAddress {
                host: "mc.example.com".to_string(),
                port: None
            }
        );
        assert_eq!(
            ServerAddress::parse("[::1]:25566").unwrap(),
            ServerAddress {
                host: "::1".to_string(),
                port: Some(25566)
            }
        );
        assert_eq!(
            ServerAddress::parse("[::1]").unwrap(),
            ServerAddress {
                host: "::1".to_string(),
                port: None
            }
        );
        assert!(ServerAddress::parse("mc.example.com:").is_err());
        assert!(ServerAddress::parse("mc.example.com:notaport").is_err());
    }

    #[test]
    fn tokenize_splits_on_space_and_respects_quotes() {
        assert_eq!(tokenize("foo bar"), vec!["foo", "bar"]);
        assert_eq!(tokenize("'foo bar' baz"), vec!["foo bar", "baz"]);
        assert_eq!(tokenize(r#""line1`nline2""#), vec!["line1\nline2"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
        assert_eq!(tokenize("   "), Vec::<String>::new());
    }

    #[test]
    fn quick_play_threshold_is_release_1_20() {
        assert!(supports_quick_play(Some("1.20.1")));
        assert!(supports_quick_play(Some("1.20")));
        assert!(!supports_quick_play(Some("1.19.4")));
        assert!(!supports_quick_play(None));
    }

    #[test]
    fn log4j_threshold_is_release_1_7() {
        assert!(is_using_log4j(Some("1.12.2")));
        assert!(is_using_log4j(Some("1.7")));
        assert!(!is_using_log4j(Some("1.6.4")));
        assert!(
            is_using_log4j(None),
            "unknown defaults to assuming a modern version, matching Java's orElse(\"1.7\")"
        );
    }

    #[test]
    fn xincgc_is_forbidden_only_on_java_9_plus() {
        assert!(is_forbidden("-Xincgc", Some(9)));
        assert!(is_forbidden("-Xincgc", Some(17)));
        assert!(!is_forbidden("-Xincgc", Some(8)));
        assert!(!is_forbidden("-Xmx4096m", Some(17)));
    }

    #[test]
    fn log4j_extraction_actually_writes_a_readable_file_and_picks_the_right_template() {
        let dir = tmp_dir("log4j_extract");

        let old_path = dir.join("old.xml");
        extract_log4j_configuration_file(&old_path, Some("1.7"), false).unwrap();
        assert_eq!(std::fs::read_to_string(&old_path).unwrap(), LOG4J_1_7);

        let modern_path = dir.join("modern.xml");
        extract_log4j_configuration_file(&modern_path, Some("1.20.1"), false).unwrap();
        assert_eq!(std::fs::read_to_string(&modern_path).unwrap(), LOG4J_1_12);

        let debug_path = dir.join("debug.xml");
        extract_log4j_configuration_file(&debug_path, Some("1.20.1"), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&debug_path).unwrap(),
            LOG4J_1_12_DEBUG
        );

        let unknown_path = dir.join("unknown.xml");
        extract_log4j_configuration_file(&unknown_path, None, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&unknown_path).unwrap(),
            LOG4J_1_7,
            "unknown version must default to the old template, matching Java's Release.ZERO"
        );

        let nested = dir.join("versions").join("1.20.1").join("log4j2.xml");
        extract_log4j_configuration_file(&nested, Some("1.20.1"), false).unwrap();
        assert!(nested.is_file());
    }

    use crate::install::GameRepository;
    use crate::java::{JavaInfo, JavaRuntime};
    use crate::platform::{Architecture, OperatingSystem, Platform};
    use crate::version::Version;

    fn fixture_version(name: &str) -> Version {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"))
    }

    fn fake_java_17() -> JavaRuntime {
        JavaRuntime {
            binary: PathBuf::from(r"C:\fake-jdk-17\bin\java.exe"),
            info: JavaInfo::new(
                Platform {
                    os: OperatingSystem::Windows,
                    arch: Architecture::X86_64,
                },
                "17.0.9",
                Some("Eclipse Adoptium".to_string()),
            ),
            is_managed: false,
            is_jdk: true,
        }
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-launch")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_auth() -> AuthInfo {
        AuthInfo {
            username: "TestPlayer".to_string(),
            uuid: Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap(),
            access_token: "fake-access-token".to_string(),
            user_type: USER_TYPE_LEGACY.to_string(),
            user_properties: "{}".to_string(),
            launch_arguments: None,
        }
    }

    #[test]
    fn launch_options_default_to_hmcl_window_resolution() {
        let options = LaunchOptions::new(".", fake_java_17());
        assert_eq!((options.width, options.height), (1280, 720));
    }

    #[test]
    fn errors_when_client_jar_is_missing() {
        let version = fixture_version("1.20.1.json");
        let root = tmp_dir("missing_jar");
        let repo = GameRepository::new(&root);
        let auth = test_auth();
        let options = LaunchOptions::new(&root, fake_java_17());
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let err =
            generate_command_line(&repo, &version, &auth, &options, &root.join("natives"), env)
                .unwrap_err();
        assert!(matches!(err, LaunchError::JarMissing(_)));
    }

    #[test]
    fn produces_a_working_command_line_for_vanilla_1_20_1() {
        let version = fixture_version("1.20.1.json");
        let root = tmp_dir("full_launch");
        let repo = GameRepository::new(&root);

        std::fs::create_dir_all(repo.version_root(&version.id)).unwrap();
        std::fs::write(repo.version_jar(&version.id), b"fake jar bytes").unwrap();

        let auth = test_auth();
        let mut options = LaunchOptions::new(&root, fake_java_17());
        options.max_memory = Some(4096);
        options.java_arguments = vec!["-Xincgc".to_string()]; // 用户手滑留的老参数, 必须被过滤掉
        let native_folder = root.join("natives");
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let generated =
            generate_command_line(&repo, &version, &auth, &options, &native_folder, env)
                .expect("should generate a command line");
        let args = generated.command.as_list();

        assert_eq!(
            args[0], r"C:\fake-jdk-17\bin\java.exe",
            "java binary must be the very first token"
        );
        assert!(
            !args.contains(&"-Xss1M".to_string()),
            "1.20.1's {{\"arch\":\"x86\"}} rule must not fire on a 64-bit JVM"
        );
        assert!(
            args.contains(&"net.minecraft.client.main.Main".to_string()),
            "main class must be present"
        );
        assert!(
            args.contains(&"-Xmx4096m".to_string()),
            "explicit max memory must produce -Xmx4096m"
        );
        assert!(
            !args.contains(&"-Xincgc".to_string()),
            "-Xincgc must be stripped on Java 9+ (it was removed from the JVM)"
        );

        let username_idx = args
            .iter()
            .position(|a| a == "--username")
            .expect("--username flag present");
        assert_eq!(
            args[username_idx + 1],
            "TestPlayer",
            "auth_player_name placeholder must resolve to the account username"
        );

        let uuid_idx = args
            .iter()
            .position(|a| a == "--uuid")
            .expect("--uuid flag present");
        assert_eq!(
            args[uuid_idx + 1],
            "0123456789abcdef0123456789abcdef",
            "auth_uuid must be the no-dash compact form"
        );

        let cp_idx = args
            .iter()
            .position(|a| a == "-cp")
            .expect("-cp flag present");
        let classpath = &args[cp_idx + 1];
        assert!(
            classpath.contains(&repo.version_jar(&version.id).to_string_lossy().to_string()),
            "classpath must include the client jar itself"
        );

        assert!(
            args.iter()
                .any(|a| a.starts_with("-Djava.library.path=") && a.contains("natives")),
            "natives directory placeholder must be substituted into java.library.path"
        );
        assert!(args.contains(&"-Dminecraft.launcher.brand=HMCL-rs".to_string()));
        assert!(args.contains(&format!(
            "-Dminecraft.launcher.version={}",
            env!("CARGO_PKG_VERSION")
        )));

        assert_eq!(
            generated.java_native_folder, native_folder,
            "no -Djava.library.path subdir override present, so it stays the plain natives folder"
        );
        assert_eq!(
            generated.temp_native_folder, None,
            "windows-gnu never needs the ASCII-path workaround"
        );
    }

    #[test]
    fn min_memory_is_dropped_when_it_would_exceed_max_memory() {
        let version = fixture_version("1.20.1.json");
        let root = tmp_dir("min_exceeds_max");
        let repo = GameRepository::new(&root);
        std::fs::create_dir_all(repo.version_root(&version.id)).unwrap();
        std::fs::write(repo.version_jar(&version.id), b"fake").unwrap();

        let auth = test_auth();
        let mut options = LaunchOptions::new(&root, fake_java_17());
        options.max_memory = Some(1024);
        options.min_memory = Some(2048);
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let generated =
            generate_command_line(&repo, &version, &auth, &options, &root.join("natives"), env)
                .unwrap();
        let args = generated.command.as_list();
        assert!(args.contains(&"-Xmx1024m".to_string()));
        assert!(
            !args.iter().any(|a| a.starts_with("-Xms")),
            "min > max must not produce an -Xms flag at all"
        );
    }

    #[test]
    fn quick_play_multiplayer_uses_new_flag_on_1_20_1() {
        let version = fixture_version("1.20.1.json");
        let root = tmp_dir("quickplay");
        let repo = GameRepository::new(&root);
        std::fs::create_dir_all(repo.version_root(&version.id)).unwrap();
        std::fs::write(repo.version_jar(&version.id), b"fake").unwrap();

        let auth = test_auth();
        let mut options = LaunchOptions::new(&root, fake_java_17());
        options.quick_play_option = Some(QuickPlayOption::MultiPlayer {
            server_ip: "play.example.com:25566".to_string(),
        });
        let env = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };

        let generated =
            generate_command_line(&repo, &version, &auth, &options, &root.join("natives"), env)
                .unwrap();
        let args = generated.command.as_list();
        let idx = args
            .iter()
            .position(|a| a == "--quickPlayMultiplayer")
            .expect("1.20.1 supports the new quickplay flag");
        assert_eq!(args[idx + 1], "play.example.com:25566");
        assert!(
            !args.contains(&"--server".to_string()),
            "must not also emit the legacy --server/--port pair"
        );
    }
}

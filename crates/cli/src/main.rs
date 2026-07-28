use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use hmcl_core::download::DownloadProvider;
use hmcl_core::install::{self, GameRepository};
use hmcl_core::java::find_a_java;
use hmcl_core::launch;
use hmcl_core::platform::Platform;
use hmcl_core::version::Env;

#[derive(Parser)]
#[command(name = "hmcl-cli", version, about = "HMCL-rs 调试用命令行前端")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// ponytail: `Launch` 明显比其它子命令重(参数多), clippy 的 large_enum_variant
// 建议给每个字段加 Box 间接层——这是一个进程生命周期内只 clap::parse() 一次的
// CLI 参数枚举, 不是热路径上反复分配的数据结构, 加 Box 纯粹是噪音。
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    Launch {
        #[arg(long, default_value = ".minecraft")]
        dir: PathBuf,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        instance: Option<String>,
        #[arg(long)]
        instance_id: Option<String>,
        #[arg(long)]
        offline: String,
        #[arg(long, value_enum, default_value_t = Source::Mojang)]
        source: Source,
        #[arg(long, default_value_t = 2048)]
        max_memory: u32,
        #[arg(long)]
        install_only: bool,
        #[arg(long, value_enum)]
        loader: Option<Loader>,
        #[arg(long)]
        loader_version: Option<String>,
        #[arg(long)]
        loader_installer: Option<PathBuf>,
        #[arg(long)]
        optifine: bool,
        #[arg(long)]
        optifine_version: Option<String>,
        #[arg(long)]
        optifine_installer: Option<PathBuf>,
        /// 装 LiteLoader（不给具体版本号就装该游戏版本时间戳最新的构建）。跟
        /// OptiFine 一样是独立于 `--loader` 的追加式开关——LiteLoader 用它自己
        /// 固定的、比 Forge 还高的优先级(60000)，可以叠加在已经选好的 `--loader`
        /// 之上，也可以单独装在纯原版上。纯网络元数据驱动，不需要本地安装器文件。
        #[arg(long)]
        liteloader: bool,
        #[arg(long)]
        liteloader_version: Option<String>,
        /// 直接指定要用的 java.exe 路径，跳过 `JAVA_HOME`/`PATH` 探测。主要用来在
        /// `hmcl-cli java install` 装好一个托管 Java（比如 Java 8）之后马上拿来试。
        #[arg(long)]
        java: Option<PathBuf>,
    },
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },
    Instance {
        #[command(subcommand)]
        action: InstanceAction,
    },
    JavaInstall {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = JavaComponentArg::JreLegacy)]
        component: JavaComponentArg,
        #[arg(long, value_enum, default_value_t = Source::Mojang)]
        source: Source,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum JavaComponentArg {
    /// Java 8——1.13 以下版本、以及依赖 `LaunchWrapper`（老版本 Forge/LiteLoader）
    /// 的游戏需要的那个。`LaunchWrapper` 假设系统类加载器是 `URLClassLoader`，
    /// Java 9+ 不再成立，这些加载器在更新的 Java 上会直接 `ClassCastException`。
    JreLegacy,
    RuntimeAlpha,
    RuntimeBeta,
    RuntimeDelta,
    RuntimeEpsilon,
}

impl From<JavaComponentArg> for hmcl_core::download::mojang_java::MojangJavaComponent {
    fn from(value: JavaComponentArg) -> Self {
        use hmcl_core::download::mojang_java::MojangJavaComponent as C;
        match value {
            JavaComponentArg::JreLegacy => C::JreLegacy,
            JavaComponentArg::RuntimeAlpha => C::RuntimeAlpha,
            JavaComponentArg::RuntimeBeta => C::RuntimeBeta,
            JavaComponentArg::RuntimeDelta => C::RuntimeDelta,
            JavaComponentArg::RuntimeEpsilon => C::RuntimeEpsilon,
        }
    }
}

#[derive(Subcommand)]
enum AccountAction {
    Add { username: String },
    List,
    Remove { account_id: String },
}

#[derive(Subcommand)]
enum InstanceAction {
    List {
        #[arg(long, default_value = ".minecraft")]
        dir: PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Loader {
    Fabric,
    Quilt,
    LegacyFabric,
    Forge,
    ForgeOld,
    NeoForge,
    Cleanroom,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Source {
    Mojang,
    Bmclapi,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hmcl_core=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Launch {
            dir,
            version,
            instance,
            instance_id,
            offline,
            source,
            max_memory,
            install_only,
            loader,
            loader_version,
            loader_installer,
            optifine,
            optifine_version,
            optifine_installer,
            liteloader,
            liteloader_version,
            java,
        } => {
            launch_command(
                dir,
                version,
                instance,
                instance_id,
                offline,
                source,
                max_memory,
                install_only,
                loader,
                loader_version,
                loader_installer,
                optifine,
                optifine_version,
                optifine_installer,
                liteloader,
                liteloader_version,
                java,
            )
            .await
        }
        Command::Account { action } => account_command(action),
        Command::Instance { action } => instance_command(action),
        Command::JavaInstall {
            dir,
            component,
            source,
        } => {
            let dir = dir.unwrap_or_else(|| hmcl_core::settings::launcher_data_dir().join("java"));
            java_install_command(dir, component, source).await
        }
    }
}

async fn java_install_command(
    dir: PathBuf,
    component: JavaComponentArg,
    source: Source,
) -> anyhow::Result<()> {
    use hmcl_core::download::mojang_java;

    let client = reqwest::Client::new();
    let provider = match source {
        Source::Mojang => DownloadProvider::mojang(),
        Source::Bmclapi => DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com"),
    };

    println!("==> 下载 Mojang JRE ({})", component_display(component));
    let runtime =
        mojang_java::install_mojang_java(&client, &provider, &dir, component.into()).await?;
    println!(
        "==> 装好了: {} (Java {})",
        runtime.binary.display(),
        runtime
            .parsed_version()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string())
    );
    println!(
        "    配合 launch 用: --java \"{}\"",
        runtime.binary.display()
    );
    Ok(())
}

fn component_display(component: JavaComponentArg) -> &'static str {
    match component {
        JavaComponentArg::JreLegacy => "jre-legacy (Java 8)",
        JavaComponentArg::RuntimeAlpha => "java-runtime-alpha (Java 16)",
        JavaComponentArg::RuntimeBeta => "java-runtime-beta (Java 17)",
        JavaComponentArg::RuntimeDelta => "java-runtime-delta (Java 21)",
        JavaComponentArg::RuntimeEpsilon => "java-runtime-epsilon (Java 25)",
    }
}

fn accounts_file_path() -> PathBuf {
    hmcl_core::settings::launcher_data_dir()
        .join("config")
        .join("accounts.json")
}

fn account_command(action: AccountAction) -> anyhow::Result<()> {
    use hmcl_core::settings::accounts::{AccountsFile, OfflineAccountEntry, SCHEMA_ID};

    let path = accounts_file_path();
    let loaded = hmcl_core::settings::load::<AccountsFile>(&path, SCHEMA_ID);
    let mut file = loaded.value;

    match action {
        AccountAction::Add { username } => {
            let entry = OfflineAccountEntry::new(&username);
            println!(
                "新建离线账户: {} (accountID={}, uuid={})",
                entry.profile_name,
                entry.account_id,
                entry.resolved_profile_id()
            );
            file.upsert_offline_account(&entry);
            if !loaded.can_save {
                anyhow::bail!("{} 的 $schema 认不出来或版本不兼容, 拒绝覆盖保存(避免冲掉更高版本 HMCL 写的内容)", path.display());
            }
            hmcl_core::settings::save(&path, SCHEMA_ID, &file)?;
            println!("已保存到 {}", path.display());
        }
        AccountAction::List => {
            let offline = file.offline_accounts();
            if offline.is_empty() {
                println!("(没有离线账户, 用 `hmcl-cli account add <用户名>` 建一个)");
            }
            for a in offline {
                println!(
                    "{}  {}  uuid={}",
                    a.account_id,
                    a.profile_name,
                    a.resolved_profile_id()
                );
            }
        }
        AccountAction::Remove { account_id } => {
            if file.remove_account(&account_id) {
                if !loaded.can_save {
                    anyhow::bail!(
                        "{} 的 $schema 认不出来或版本不兼容, 拒绝覆盖保存",
                        path.display()
                    );
                }
                hmcl_core::settings::save(&path, SCHEMA_ID, &file)?;
                println!("已删除 {account_id}");
            } else {
                println!("没找到 {account_id}");
            }
        }
    }
    Ok(())
}

fn instance_command(action: InstanceAction) -> anyhow::Result<()> {
    match action {
        InstanceAction::List { dir } => {
            let repo = GameRepository::new(&dir);
            let all = repo.load_all_versions();
            if all.is_empty() {
                println!("(在 {} 下没找到任何实例)", dir.display());
                return Ok(());
            }
            let mut ids: Vec<&String> = all.keys().collect();
            ids.sort();
            for id in ids {
                let raw = &all[id];
                match raw.resolve(&all) {
                    Ok(resolved) => {
                        let inherits = raw
                            .inherits_from
                            .as_deref()
                            .map(|p| format!(", 继承自 {p}"))
                            .unwrap_or_default();
                        println!(
                            "{id}{inherits}  mainClass={}",
                            resolved.main_class.as_deref().unwrap_or("?")
                        );
                    }
                    Err(e) => println!("{id}  ! 解析失败: {e}"),
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn launch_command(
    dir: PathBuf,
    version_id: Option<String>,
    instance_id_to_launch: Option<String>,
    save_as_instance_id: Option<String>,
    offline_username: String,
    source: Source,
    max_memory: u32,
    install_only: bool,
    loader: Option<Loader>,
    loader_version: Option<String>,
    loader_installer: Option<PathBuf>,
    optifine: bool,
    optifine_version: Option<String>,
    optifine_installer: Option<PathBuf>,
    liteloader: bool,
    liteloader_version: Option<String>,
    java_override: Option<PathBuf>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let provider = match source {
        Source::Mojang => DownloadProvider::mojang(),
        Source::Bmclapi => DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com"),
    };
    let repo = GameRepository::new(&dir);
    let cache = hmcl_core::download::CacheRepository::new(dir.join(".hmcl-rs-cache"));
    let env = Env {
        platform: Platform::CURRENT,
        os_version: "",
    };

    if version_id.is_some() == instance_id_to_launch.is_some() {
        anyhow::bail!("必须且只能指定 --version 或 --instance 之一");
    }

    if let Some(instance_id) = instance_id_to_launch {
        println!("==> 从已保存的实例启动: {instance_id}");
        let all = repo.load_all_versions();
        let raw = all.get(&instance_id).ok_or_else(|| {
            anyhow::anyhow!(
                "没有找到实例 {instance_id}(用 `hmcl-cli instance list --dir {}` 看看有哪些)",
                dir.display()
            )
        })?;
        let version = raw.resolve(&all)?;
        return install_and_launch(
            &client,
            &provider,
            &cache,
            &repo,
            &dir,
            env,
            version,
            offline_username,
            max_memory,
            install_only,
            java_override,
        )
        .await;
    }
    let version_id = version_id.expect("validated above: exactly one of version/instance is set");

    println!("==> 拉取版本清单");
    let manifest = install::fetch_version_manifest(&client, &provider).await?;
    let entry = manifest
        .find(&version_id)
        .ok_or_else(|| anyhow::anyhow!("version {version_id} not found in manifest"))?;

    println!("==> 下载 version.json ({})", entry.url);
    let mut raw_version =
        install::download_version_json(&client, &provider, &repo, &version_id, entry).await?;

    match loader {
        Some(Loader::Fabric) => {
            use hmcl_core::download::fabric;
            let meta = match &loader_version {
                Some(v) => {
                    println!("==> 拉取 Fabric loader {v} 元数据");
                    fabric::fetch_loader_meta(&client, &provider, &version_id, v).await?
                }
                None => {
                    println!("==> 拉取 {version_id} 可用的最新 Fabric loader");
                    fabric::fetch_latest_build(&client, &provider, &version_id).await?
                }
            };
            println!(
                "    Fabric Loader {} (intermediary {})",
                meta.loader.version, meta.intermediary.maven
            );
            raw_version.patches = Some(vec![fabric::build_patch(&meta)]);
        }
        Some(Loader::Quilt) => {
            use hmcl_core::download::quilt;
            let meta = match &loader_version {
                Some(v) => {
                    println!("==> 拉取 Quilt loader {v} 元数据");
                    quilt::fetch_loader_meta(&client, &provider, &version_id, v).await?
                }
                None => {
                    println!("==> 拉取 {version_id} 可用的最新 Quilt loader");
                    quilt::fetch_latest_build(&client, &provider, &version_id).await?
                }
            };
            println!("    Quilt Loader {}", meta.loader.version);
            raw_version.patches = Some(vec![quilt::build_patch(&meta)]);
        }
        Some(Loader::LegacyFabric) => {
            use hmcl_core::download::legacyfabric;
            let meta = match &loader_version {
                Some(v) => {
                    println!("==> 拉取 LegacyFabric loader {v} 元数据");
                    legacyfabric::fetch_loader_meta(&client, &provider, &version_id, v).await?
                }
                None => {
                    println!("==> 拉取 {version_id} 可用的最新 LegacyFabric loader");
                    legacyfabric::fetch_latest_build(&client, &provider, &version_id).await?
                }
            };
            println!("    LegacyFabric Loader {}", meta.loader.version);
            raw_version.patches = Some(vec![legacyfabric::build_patch(&meta)]);
        }
        Some(Loader::Forge) => {
            use hmcl_core::download::forge;

            let installer_path = match &loader_installer {
                Some(p) => p.clone(),
                None => {
                    let build = match &loader_version {
                        Some(v) => {
                            forge::fetch_build_by_version(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                                v,
                            )
                            .await?
                        }
                        None => {
                            println!("==> 拉取 {version_id} 可用的最新 Forge 构建");
                            forge::fetch_latest_build(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                            )
                            .await?
                        }
                    };
                    println!("    Forge {}", build.version);
                    let dest = installer_cache_path(&dir, "forge", &build.version);
                    println!("==> 下载 Forge 安装器: {}", build.installer_url);
                    forge::download_installer(&client, &provider, &build, &dest).await?;
                    dest
                }
            };

            // Forge 的 processors 要读原版 client.jar (SRG 反混淆/打补丁的输入), 必须
            // 先把它下到位——跟 Fabric/Quilt/LegacyFabric 不一样, 那几个纯粹是拼 JSON,
            // 完全不碰文件系统。
            println!("==> 安装 Forge 需要先装好原版 client.jar");
            install::install_client_jar(&client, &provider, &cache, &repo, &raw_version).await?;

            println!("==> 探测本机 Java (Forge processor 装的时候就要跑 java)");
            let java = find_a_java(java_override.as_deref())?;
            println!("    用 {} 执行 Forge processors", java.binary.display());

            let self_version = loader_version.clone().unwrap_or_else(|| {
                installer_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("forge")
                    .to_string()
            });

            println!("==> 运行 Forge 安装器: {}", installer_path.display());
            let patch = forge::install_new_forge(
                &client,
                &provider,
                &cache,
                &repo,
                &installer_path,
                &raw_version,
                &java.binary,
                forge::PATCH_ID,
                &self_version,
            )
            .await?;
            println!(
                "    Forge patch mainClass = {}",
                patch.main_class.as_deref().unwrap_or("?")
            );
            raw_version.patches = Some(vec![patch]);
        }
        Some(Loader::ForgeOld) => {
            use hmcl_core::download::{forge, forge_old};

            let installer_path = match &loader_installer {
                Some(p) => p.clone(),
                None => {
                    let build = match &loader_version {
                        Some(v) => {
                            forge::fetch_build_by_version(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                                v,
                            )
                            .await?
                        }
                        None => {
                            println!("==> 拉取 {version_id} 可用的最新 Forge 构建");
                            forge::fetch_latest_build(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                            )
                            .await?
                        }
                    };
                    println!("    Forge {}", build.version);
                    let dest = installer_cache_path(&dir, "forge-old", &build.version);
                    println!("==> 下载 Forge 安装器: {}", build.installer_url);
                    forge::download_installer(&client, &provider, &build, &dest).await?;
                    dest
                }
            };

            // 旧版 Forge 安装完全不碰原版 client.jar、也不需要跑任何外部程序,
            // 不用像新版那样先装 client.jar / 探测 Java。
            let self_version = loader_version.clone().unwrap_or_else(|| {
                installer_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("forge")
                    .to_string()
            });

            println!("==> 运行 Forge 旧版安装器: {}", installer_path.display());
            let patch = forge_old::install_old_forge(
                &client,
                &provider,
                &cache,
                &repo,
                &installer_path,
                &self_version,
            )
            .await?;
            println!(
                "    Forge patch mainClass = {}",
                patch.main_class.as_deref().unwrap_or("?")
            );
            raw_version.patches = Some(vec![patch]);
        }
        Some(Loader::NeoForge) => {
            use hmcl_core::download::neoforge;

            let installer_path = match &loader_installer {
                Some(p) => p.clone(),
                None => {
                    let build = match &loader_version {
                        Some(v) => {
                            neoforge::fetch_build_by_version(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                                v,
                            )
                            .await?
                        }
                        None => {
                            println!("==> 拉取 {version_id} 可用的最新 NeoForge 构建");
                            neoforge::fetch_latest_build(
                                &client,
                                hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                                &version_id,
                            )
                            .await?
                        }
                    };
                    println!("    NeoForge {}", build.version);
                    let dest = installer_cache_path(&dir, "neoforge", &build.version);
                    println!("==> 下载 NeoForge 安装器: {}", build.installer_url);
                    neoforge::download_installer(&client, &provider, &build, &dest).await?;
                    dest
                }
            };

            println!("==> 安装 NeoForge 需要先装好原版 client.jar");
            install::install_client_jar(&client, &provider, &cache, &repo, &raw_version).await?;

            println!("==> 探测本机 Java (NeoForge processor 装的时候就要跑 java)");
            let java = find_a_java(java_override.as_deref())?;
            println!("    用 {} 执行 NeoForge processors", java.binary.display());

            println!("==> 运行 NeoForge 安装器: {}", installer_path.display());
            let patch = neoforge::install_neoforge(
                &client,
                &provider,
                &cache,
                &repo,
                &installer_path,
                &raw_version,
                &java.binary,
            )
            .await?;
            println!(
                "    NeoForge patch version = {}, mainClass = {}",
                patch.version.as_deref().unwrap_or("?"),
                patch.main_class.as_deref().unwrap_or("?")
            );
            raw_version.patches = Some(vec![patch]);
        }
        Some(Loader::Cleanroom) => {
            use hmcl_core::download::cleanroom;

            let installer_path = match &loader_installer {
                Some(p) => p.clone(),
                None => {
                    let build = match &loader_version {
                        Some(v) => {
                            cleanroom::fetch_build_by_version(&client, &version_id, v).await?
                        }
                        None => {
                            println!("==> 拉取 {version_id} 可用的最新 Cleanroom 构建");
                            cleanroom::fetch_latest_build(&client, &version_id).await?
                        }
                    };
                    println!("    Cleanroom {}", build.version);
                    let dest = installer_cache_path(&dir, "cleanroom", &build.version);
                    println!("==> 下载 Cleanroom 安装器: {}", build.installer_url);
                    cleanroom::download_installer(&client, &build, &dest).await?;
                    dest
                }
            };

            println!("==> 安装 Cleanroom 需要先装好原版 client.jar");
            install::install_client_jar(&client, &provider, &cache, &repo, &raw_version).await?;

            println!("==> 探测本机 Java (以防装的时候要跑 processor)");
            let java = find_a_java(java_override.as_deref())?;

            println!("==> 运行 Cleanroom 安装器: {}", installer_path.display());
            let patch = cleanroom::install_cleanroom(
                &client,
                &provider,
                &cache,
                &repo,
                &installer_path,
                &raw_version,
                &java.binary,
            )
            .await?;
            println!(
                "    Cleanroom patch version = {}, mainClass = {}",
                patch.version.as_deref().unwrap_or("?"),
                patch.main_class.as_deref().unwrap_or("?")
            );
            raw_version.patches = Some(vec![patch]);
        }
        None => {}
    }

    let empty_provider: std::collections::HashMap<String, hmcl_core::version::Version> =
        std::collections::HashMap::new();

    let had_optifine = optifine || optifine_installer.is_some();
    if had_optifine {
        use hmcl_core::download::optifine;

        let optifine_path = match &optifine_installer {
            Some(p) => p.clone(),
            None => {
                let build = match &optifine_version {
                    Some(v) => {
                        optifine::fetch_build_by_version(
                            &client,
                            hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                            &version_id,
                            v,
                        )
                        .await?
                    }
                    None => {
                        println!("==> 拉取 {version_id} 可用的最新 OptiFine 构建");
                        optifine::fetch_latest_build(
                            &client,
                            hmcl_core::download::DEFAULT_BMCLAPI_API_ROOT,
                            &version_id,
                        )
                        .await?
                    }
                };
                println!("    OptiFine {}", build.version);
                let dest = installer_cache_path(&dir, "optifine", &build.version);
                println!("==> 下载 OptiFine 安装器: {}", build.download_url);
                optifine::download_installer(&client, &build, &dest).await?;
                dest
            }
        };

        // OptiFine 惯例上"应该最后装": 先按目前已经选好的 loader(可能是 Forge,
        // 也可能什么都没选就是纯原版)把版本解析出来, 拿它的 mainClass 判断兼容性,
        // 再把 OptiFine 的 patch 追加到 patches 列表末尾, 而不是替换掉已有的 patch。
        let pre_optifine = raw_version.resolve(&empty_provider)?;

        println!("==> 安装 OptiFine 前先确保原版 client.jar 已就绪");
        install::install_client_jar(&client, &provider, &cache, &repo, &raw_version).await?;

        println!("==> 探测本机 Java (部分 OptiFine 构建装的时候要跑内置的 Patcher)");
        let java = find_a_java(java_override.as_deref())?;

        println!("==> 运行 OptiFine 安装器: {}", optifine_path.display());
        let patch = optifine::install_optifine(
            &repo,
            &optifine_path,
            &pre_optifine.id,
            pre_optifine.main_class.as_deref().unwrap_or(""),
            &java.binary,
        )
        .await?;
        println!(
            "    OptiFine patch version = {}",
            patch.version.as_deref().unwrap_or("?")
        );

        raw_version.patches.get_or_insert_with(Vec::new).push(patch);
    }

    if liteloader {
        use hmcl_core::download::liteloader;

        let (repo_url, build) = match &liteloader_version {
            Some(v) => {
                println!("==> 拉取 LiteLoader {v} 元数据");
                liteloader::fetch_build_by_version(&client, &provider, &version_id, v).await?
            }
            None => {
                println!("==> 拉取 {version_id} 可用的最新 LiteLoader 构建");
                liteloader::fetch_latest_build(&client, &provider, &version_id).await?
            }
        };
        println!("    LiteLoader {}", build.version);
        raw_version
            .patches
            .get_or_insert_with(Vec::new)
            .push(liteloader::build_patch(&version_id, &repo_url, &build));
    }

    let has_patches = raw_version.patches.as_ref().is_some_and(|p| !p.is_empty());
    let version = if has_patches {
        let instance_id = save_as_instance_id
            .unwrap_or_else(|| default_instance_id(&version_id, loader, had_optifine, liteloader));
        let mut instance = hmcl_core::version::Version::new(&instance_id);
        instance.inherits_from = Some(version_id.clone());
        instance.patches = raw_version.patches.take();
        repo.save_version_json(&instance)?;
        println!("==> 已保存为实例: {instance_id} (inheritsFrom {version_id})");

        let mut provider_map = std::collections::HashMap::new();
        provider_map.insert(version_id.clone(), raw_version);
        provider_map.insert(instance_id, instance.clone());
        instance.resolve(&provider_map)?
    } else {
        raw_version.resolve(&empty_provider)?
    };
    if loader.is_some() || had_optifine || liteloader {
        println!(
            "    合并后 mainClass = {}",
            version.main_class.as_deref().unwrap_or("?")
        );
    }

    install_and_launch(
        &client,
        &provider,
        &cache,
        &repo,
        &dir,
        env,
        version,
        offline_username,
        max_memory,
        install_only,
        java_override,
    )
    .await
}

/// 薄包装：真正的编排逻辑在 `hmcl_core::session::install_and_launch`（GUI 也会
///调它，不能让这份实现只活在 CLI 里）。这里只管把编排过程中的事件打印出来、
/// 把启动完的进程接上 stdout/stderr 转发, 这两件事都是"人怎么看进度", 不属于
/// 编排本身。
#[allow(clippy::too_many_arguments)]
async fn install_and_launch(
    client: &reqwest::Client,
    provider: &DownloadProvider,
    cache: &hmcl_core::download::CacheRepository,
    repo: &GameRepository,
    dir: &Path,
    env: Env<'_>,
    version: hmcl_core::version::Version,
    offline_username: String,
    max_memory: u32,
    install_only: bool,
    java_override: Option<PathBuf>,
) -> anyhow::Result<()> {
    use hmcl_core::session::{self, LaunchEvent, LaunchRequest};
    let uuid = hmcl_core::auth::offline_player_uuid(&offline_username);

    println!("==> 安装 client.jar + libraries + assets (可能需要下载几十到几百 MB, 请耐心等)");
    let req = LaunchRequest {
        client,
        provider,
        cache,
        repo,
        dir,
        env,
        version,
        auth: hmcl_core::launch::AuthInfo {
            username: offline_username,
            uuid,
            access_token: uuid.simple().to_string(),
            user_type: hmcl_core::launch::USER_TYPE_LEGACY.to_string(),
            user_properties: "{}".to_string(),
            launch_arguments: None,
        },
        default_max_memory: max_memory,
        default_auto_memory: false,
        default_min_memory: None,
        default_metaspace: None,
        default_window_width: 1280,
        default_window_height: 720,
        default_fullscreen: false,
        default_debug_log_output: false,
        default_no_jvm_options: false,
        default_no_optimizing_jvm_options: false,
        default_jvm_options: None,
        default_game_arguments: None,
        default_quick_play_option: None,
        quick_play_override: None,
        default_wrapper: None,
        default_process_priority: Default::default(),
        default_graphics_backend: Default::default(),
        default_environment_variables: None,
        default_pre_launch_command: None,
        default_post_exit_command: None,
        default_use_custom_natives: false,
        default_natives_directory: None,
        install_only,
        java_override,
    };
    let process = session::install_and_launch(req, |event| match event {
        LaunchEvent::InstallSummary(report) => {
            let lib_failures: Vec<_> = report
                .library_results
                .iter()
                .filter(|(_, r)| r.is_err())
                .collect();
            let obj_failures: Vec<_> = report
                .object_results
                .iter()
                .filter(|(_, r)| r.is_err())
                .collect();
            println!(
                "    libraries: {} 成功 / {} 失败, assets: {} 成功 / {} 失败",
                report.library_results.len() - lib_failures.len(),
                lib_failures.len(),
                report.object_results.len() - obj_failures.len(),
                obj_failures.len()
            );
            for (path, err) in lib_failures.iter().chain(obj_failures.iter()) {
                if let Err(e) = err {
                    eprintln!("    ! {path:?}: {e}");
                }
            }
        }
        LaunchEvent::JavaDetected(java) => {
            println!("==> 探测本机 Java");
            println!(
                "    用 {} (Java {})",
                java.binary.display(),
                java.parsed_version()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".to_string())
            );
        }
        LaunchEvent::CommandLine(line) => {
            println!("==> 生成启动命令行");
            println!("    {line}");
        }
        LaunchEvent::Warning(message) => println!("==> 警告: {message}"),
    })
    .await?;

    let Some(launched) = process else {
        println!("==> --install-only, 不启动");
        return Ok(());
    };
    let mut process = launched.process;

    println!("==> 解压 natives + 启动进程");
    let stdout = process.child.stdout.take().unwrap();
    let stderr = process.child.stderr.take().unwrap();
    let stdout_task = tokio::spawn(launch::pump_lines(stdout, |line| println!("[game] {line}")));
    let stderr_task = tokio::spawn(launch::pump_lines(stderr, |line| {
        eprintln!("[game] {line}")
    }));

    let status = process.wait().await?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    println!("==> 游戏进程退出, 状态: {status}");
    if let Some(post_exit) = launched.post_exit_command {
        println!("==> 执行 postExitCommand");
        if let Err(e) = session::run_user_command(&post_exit).await {
            eprintln!("    ! postExitCommand 执行失败: {e}");
        }
    }
    Ok(())
}

fn default_instance_id(
    version_id: &str,
    loader: Option<Loader>,
    had_optifine: bool,
    liteloader: bool,
) -> String {
    let mut id = version_id.to_string();
    if let Some(loader) = loader {
        id.push('-');
        id.push_str(loader_slug(loader));
    }
    if had_optifine {
        id.push_str("-optifine");
    }
    if liteloader {
        id.push_str("-liteloader");
    }
    id
}

fn loader_slug(loader: Loader) -> &'static str {
    match loader {
        Loader::Fabric => "fabric",
        Loader::Quilt => "quilt",
        Loader::LegacyFabric => "legacyfabric",
        Loader::Forge | Loader::ForgeOld => "forge",
        Loader::NeoForge => "neoforge",
        Loader::Cleanroom => "cleanroom",
    }
}

fn installer_cache_path(game_dir: &Path, loader: &str, version: &str) -> PathBuf {
    game_dir
        .join(".hmcl-rs-cache")
        .join("installers")
        .join(format!("{loader}-{version}-installer.jar"))
}

# HMCL-Rust

> [!WARNING]
> **此项目 100% 的代码由 AI 生成。如果你觉得这是垃圾代码，那就是。**
> 
> 本项目仍处于开发阶段，不保证稳定性，也不保证不会损坏游戏文件或存档。请自行备份重要数据。

这是一个参考 HMCL 实现、使用 Rust 和 Slint 编写的非官方 Minecraft Java 版启动器，属于 HMCL 的 Rust 重实现/衍生项目，目标是在保留 HMCL 主要使用体验的同时提供原生 Windows 桌面界面。

本项目与 HMCL-dev、Mojang Studios、Microsoft 或 Modrinth 均无隶属、授权或背书关系。

## License

本项目选择 **[GNU General Public License v3.0 only](https://www.gnu.org/licenses/gpl-3.0.html)**（SPDX：`GPL-3.0-only`）。

选择 GPL-3.0-only 的主要原因：

- [HMCL](https://github.com/HMCL-dev/HMCL) 使用 GPLv3，并附带 GPLv3 第 7 节允许的附加条款。
- 本项目使用的 [Slint](https://github.com/slint-ui/slint/blob/master/LICENSE.md) 提供 GPLv3 开源许可选项。
- Rust 依赖仍分别受其自身许可证约束；多数依赖采用 MIT、Apache-2.0 或二者任选其一的许可证。
- Modrinth API 及其平台内容的使用还须遵守 [Modrinth Terms](https://modrinth.com/legal/terms)，下载到的项目仍受各自作者所选许可证约束。

本项目作为参考 HMCL 实现的非官方 Rust 重实现/衍生项目，按保守原则遵守 HMCL 在 GPLv3 第 7 节下公布的附加条款：

1. 必须以合理方式修改软件名称或版本号，使其与原始软件不同。
2. 不得移除软件中显示的版权声明。

本节只是项目许可说明，不构成法律意见。

## 已完成且正常的功能

以下内容是当前已实现，并经过自动化测试或日常手动测试的主要流程：

- Minecraft 游戏版本浏览、筛选、下载与安装。
- Forge、NeoForge、OptiFine、Fabric 和 Quilt 加载器的安装、卸载与切换。
- 游戏实例列表、搜索、重命名、复制、删除、设置及多游戏文件夹管理。
- 实例名称和图标随模组加载器变化自动更新。
- 离线账户创建、账户选择、默认 Steve 头像及 UUID 复制。
- 游戏启动、启动进度、取消启动、启动脚本生成，以及启动游戏后保持显示、最小化或退出启动器。
- 游戏崩溃提示、日志等级高亮及大量日志的按需渲染。
- Java 自动检测、管理、下载与选择。
- Modrinth 模组、资源包、光影和整合包的搜索、浏览、下载与安装。
- 本地模组、资源包和光影的安装、启用、停用及在线更新检查。
- 世界列表、详情、重命名、复制、删除、导入导出、备份和数据包管理。
- Modrinth 整合包导入、导出及共享资源清理。
- 全局设置和实例设置即时生效。
- 并发下载、下载源测速选择、下载进度和完成提示。
- 在下载设置中清除各游戏文件夹的安装、在线内容与图标缓存，以及启动器下载缓存。
- 用户数据保存在 `%APPDATA%\.hmcl-rs`，并可从旧版同目录 `.hmcl` 数据文件夹复制迁移。

“正常”仅表示当前覆盖的主要使用路径可工作，不代表所有 Minecraft 版本、模组加载器版本和第三方内容组合都经过验证。

## 未知是否正常和明显不正常的功能

- **微软账户登录：** 流程已实现，但正式构建默认不包含 Microsoft Client ID，因此未经配置时无法使用；登录、令牌续期和皮肤头像流程仍需更充分的真实账户测试。
- **外置登录：** 已实现 authlib-injector 登录流程，但目前缺少可控测试服务器，尚未充分验证。
- **旧版 Minecraft：** 部分旧版本需要的虚拟资源映射尚未实现，较老版本可能无法正常启动或缺少声音、语言等资源。
- **跨平台支持：** 当前主要面向 Windows；Linux 和 macOS 尚未完成构建、界面和启动流程验证。
- **第三方服务变化：** Microsoft、Mojang、Modrinth 或认证服务器接口发生变化时，相关功能可能失效。
- **不支持 CurseForge：** 在线内容仅接入 Modrinth；世界在线下载也不在当前计划内。
- **图形兼容性：** GUI 仅保留 Slint 的 FemtoVG/OpenGL 渲染器，不包含软件渲染后备；缺少可用 OpenGL 驱动的环境可能无法启动界面。

发现问题时，请先备份 `.minecraft`、世界存档和 `%APPDATA%\.hmcl-rs`。

## 构建指南

目前主要支持 Windows x86-64 GNU 工具链，建议直接使用 [MSYS2](https://www.msys2.org/) 准备完整环境。

安装 MSYS2 后打开 **MSYS2 MINGW64** 终端，先更新系统并安装构建工具：

```bash
pacman -Syu
# 按提示关闭并重新打开 MINGW64 终端后继续
pacman -S --needed git mingw-w64-x86_64-toolchain mingw-w64-x86_64-rust
```

确认 `rustc -vV` 显示的 host 为 `x86_64-pc-windows-gnu`，并且 `gcc`、`windres` 和 `cargo` 均可直接调用。首次构建需要联网下载 Cargo 依赖。

在 MINGW64 终端进入项目根目录后运行：

```bash
# 构建并运行 debug GUI
cargo run -p hmcl-gui

# 构建 release GUI
cargo build --release -p hmcl-gui

# 运行整个 workspace 的测试
cargo test --workspace
```

构建产物分别位于：

- Debug：`target\debug\hmcl-gui.exe`
- Release：`target\release\hmcl-gui.exe`

只有在依赖已经完整缓存时才应添加 `--offline`；全新环境首次构建不要使用它。调试版会保留控制台输出，release 版从资源管理器启动时不显示命令行窗口。

GUI 构建仅启用 Slint 的 Winit 与 FemtoVG/OpenGL 后端，不包含 Skia 或软件渲染器。运行环境需要提供可用的 OpenGL 驱动。

## Q&A

### 这是什么？

这是 [Hello Minecraft! Launcher（HMCL）](https://github.com/HMCL-dev/HMCL) 的 Rust 重写版本。由于原版使用 Java 作为底层，运行和渲染效率低下；官方虽有 [HMCLauncher-rs](https://github.com/HMCL-dev/HMCLauncher-rs) 仓库，但是似乎没有有效构建，所以我使用 AI 花了几天时间重写了此版本。

### 没人在意启动器流不流畅，游戏本身也需要 Java 启动，有什么意义吗？

原版打开在线 Mod 列表，仅 50 个条目就有高达 500+ MB 的内存占用，且浏览帧率可低至个位数。此 Rust 重写可将内存使用控制在 200 MB 以内并保证几乎无掉帧，且优化了一点点界面布局。我认为这绝对是使用体验上的提升，无论你在不在乎，至少我在乎。并且追求优化无论怎么想都不是坏事。

### 大家已经有更好的启动器用，我凭什么用你的重写版？

你想用什么就用什么，这个我自己用，做这个也是自娱自乐玩，反正这个月 AI 几乎免费。

### 为什么没有 CurseForge 源 / 微软登录？

[CurseForge](https://www.curseforge.com/) 目前必须申请 API Key 才可连接，我没兴趣接入它，另一个启动器好像也没有，就更没兴趣了。没有微软登录是因为我不想和 Macro\$hit 打交道。实际上，在调试版本中可以开启基于旧版端点和公开 `client_id` 的微软登录方式。但如上面所见，我不想和 Macro\$hit 打交道。

### 你对 HMCL 官方的想法是？

没什么想法。如果官方对这个仓库感兴趣，且有兴趣持续开发，那我把这个仓库送到他们组织下都没问题。

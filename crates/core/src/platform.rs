use std::fmt;

/// 让子进程不要新建控制台窗口。**所有**拉起 `java.exe` 的地方都要过这一道。
///
/// GUI 版是 `windows_subsystem = "windows"`，进程自己没有控制台。这时候用
/// CreateProcess 启动一个控制台子系统的程序（`java.exe`、Forge/NeoForge 的
/// processor、OptiFine 的 patcher…），Windows 会**给它新建一个控制台窗口**——
/// 表现就是启动游戏、打 patch 的时候黑框乱闪。
///
/// `CREATE_NO_WINDOW` 只是不给它建窗口，stdout/stderr 的管道照常工作，所以我们
/// 依然能把游戏日志读出来。
#[cfg(windows)]
pub fn hide_console_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
pub fn hide_console_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    command
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatingSystem {
    Windows,
    Linux,
    MacOs,
    Unknown,
}

impl OperatingSystem {
    pub fn mojang_name(self) -> &'static str {
        match self {
            OperatingSystem::Windows => "windows",
            OperatingSystem::Linux => "linux",
            OperatingSystem::MacOs => "osx",
            OperatingSystem::Unknown => "universal",
        }
    }

    pub fn parse(name: &str) -> OperatingSystem {
        let name = name.trim().to_lowercase();
        if name.contains("mac") || name.contains("darwin") || name.contains("osx") {
            OperatingSystem::MacOs
        } else if name.contains("win") {
            OperatingSystem::Windows
        } else if name.contains("solaris")
            || name.contains("linux")
            || name.contains("unix")
            || name.contains("sunos")
        {
            OperatingSystem::Linux
        } else {
            OperatingSystem::Unknown
        }
    }

    #[cfg(target_os = "windows")]
    pub const CURRENT: OperatingSystem = OperatingSystem::Windows;
    #[cfg(target_os = "linux")]
    pub const CURRENT: OperatingSystem = OperatingSystem::Linux;
    #[cfg(target_os = "macos")]
    pub const CURRENT: OperatingSystem = OperatingSystem::MacOs;
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    pub const CURRENT: OperatingSystem = OperatingSystem::Unknown;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bits {
    Bit32,
    Bit64,
    Unknown,
}

impl Bits {
    pub fn as_str(self) -> &'static str {
        match self {
            Bits::Bit32 => "32",
            Bits::Bit64 => "64",
            Bits::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Architecture {
    X86,
    X86_64,
    Arm32,
    Arm64,
    Unknown,
}

impl Architecture {
    pub fn bits(self) -> Bits {
        match self {
            Architecture::X86 | Architecture::Arm32 => Bits::Bit32,
            Architecture::X86_64 | Architecture::Arm64 => Bits::Bit64,
            Architecture::Unknown => Bits::Unknown,
        }
    }

    pub fn checked_name(self) -> &'static str {
        match self {
            Architecture::X86 => "x86",
            Architecture::X86_64 => "x86_64",
            Architecture::Arm32 => "arm32",
            Architecture::Arm64 => "arm64",
            Architecture::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Architecture {
        match value.trim().to_lowercase().as_str() {
            "x8664" | "x86-64" | "x86_64" | "amd64" | "ia32e" | "em64t" | "x64" | "intel64" => {
                Architecture::X86_64
            }
            "x8632" | "x86-32" | "x86_32" | "x86" | "i86pc" | "i386" | "i486" | "i586" | "i686"
            | "ia32" | "x32" => Architecture::X86,
            "arm64" | "aarch64" => Architecture::Arm64,
            "arm" | "arm32" => Architecture::Arm32,
            other if other.starts_with("armv7") => Architecture::Arm32,
            other if other.starts_with("armv8") || other.starts_with("armv9") => {
                Architecture::Arm64
            }
            _ => Architecture::Unknown,
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub const CURRENT: Architecture = Architecture::X86_64;
    #[cfg(target_arch = "x86")]
    pub const CURRENT: Architecture = Architecture::X86;
    #[cfg(target_arch = "aarch64")]
    pub const CURRENT: Architecture = Architecture::Arm64;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    pub const CURRENT: Architecture = Architecture::Unknown;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Platform {
    pub os: OperatingSystem,
    pub arch: Architecture,
}

impl Platform {
    pub const CURRENT: Platform = Platform {
        os: OperatingSystem::CURRENT,
        arch: Architecture::CURRENT,
    };

    pub const WINDOWS_X64: Platform = Platform {
        os: OperatingSystem::Windows,
        arch: Architecture::X86_64,
    };
    pub const LINUX_X64: Platform = Platform {
        os: OperatingSystem::Linux,
        arch: Architecture::X86_64,
    };
    pub const MACOS_ARM64: Platform = Platform {
        os: OperatingSystem::MacOs,
        arch: Architecture::Arm64,
    };
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os.mojang_name(), self.arch.checked_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_os_names() {
        assert_eq!(
            OperatingSystem::parse("Windows 11"),
            OperatingSystem::Windows
        );
        assert_eq!(OperatingSystem::parse("Mac OS X"), OperatingSystem::MacOs);
        assert_eq!(OperatingSystem::parse("Linux"), OperatingSystem::Linux);
        assert_eq!(OperatingSystem::parse("SunOS"), OperatingSystem::Linux);
        assert_eq!(OperatingSystem::parse("BeOS"), OperatingSystem::Unknown);
    }

    #[test]
    fn parses_common_arch_names() {
        assert_eq!(Architecture::parse("amd64"), Architecture::X86_64);
        assert_eq!(Architecture::parse("aarch64"), Architecture::Arm64);
        assert_eq!(Architecture::parse("i686"), Architecture::X86);
        assert_eq!(Architecture::parse("armv7l"), Architecture::Arm32);
    }
}

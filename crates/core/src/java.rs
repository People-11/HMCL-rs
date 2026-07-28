use std::path::PathBuf;

use crate::platform::{Architecture, Bits, OperatingSystem, Platform};

#[derive(Debug, thiserror::Error)]
pub enum JavaInfoError {
    #[error("unknown operating system in release file: {0:?}")]
    UnknownOperatingSystem(String),
    #[error("unknown architecture in release file: {0:?}")]
    UnknownArchitecture(String),
    #[error("release file is missing JAVA_VERSION")]
    MissingJavaVersion,
}

#[derive(Debug, thiserror::Error)]
pub enum JavaDetectError {
    #[error("no java executable found via JAVA_HOME or PATH; provide an explicit path")]
    NotFound,
    #[error("{0} has no bin/../ parent directory")]
    NoHomeDirectory(PathBuf),
    #[error("failed to read {path}/release: {source}")]
    ReleaseFileUnreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Info(#[from] JavaInfoError),
}

/// 对应 Java `KeyValuePairUtils.loadProperties`：解析形如 `KEY="VALUE"`（也接受不带
/// 引号的 `KEY=VALUE`）的行，`#` 开头的整行是注释。带引号的值里认 `\n \r \t \f \b`
/// 这几种转义，其它反斜杠转义原样吞掉反斜杠只留后面那个字符——这不是标准 shell
/// 转义规则，是 JDK `release` 文件生成器自己的写法，照抄。
fn load_properties(text: &str) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();

    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }

        let chars: Vec<char> = line.chars().collect();
        let Some(idx) = chars.iter().position(|&c| c == '=') else {
            continue;
        };
        if idx == 0 {
            continue;
        }

        let name: String = chars[..idx].iter().collect();

        let value =
            if chars.len() > idx + 2 && chars[idx + 1] == '"' && chars[chars.len() - 1] == '"' {
                let inner = &chars[idx + 2..chars.len() - 1];
                if !inner.contains(&'\\') {
                    inner.iter().collect()
                } else {
                    let mut out = String::new();
                    let mut i = 0;
                    while i < inner.len() {
                        let ch = inner[i];
                        if ch == '\\' && i < inner.len() - 1 {
                            i += 1;
                            out.push(match inner[i] {
                                'n' => '\n',
                                'r' => '\r',
                                't' => '\t',
                                'f' => '\u{0C}',
                                'b' => '\u{08}',
                                other => other,
                            });
                        } else {
                            out.push(ch);
                        }
                        i += 1;
                    }
                    out
                }
            } else {
                chars[idx + 1..].iter().collect()
            };

        result.insert(name, value);
    }

    result
}

pub fn parse_major_version(version: &str) -> Option<u32> {
    let start = if let Some(rest) = version.strip_prefix("1.") {
        version.len() - rest.len()
    } else {
        0
    };
    let bytes = version.as_bytes();
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end > start {
        version[start..end].parse().ok()
    } else {
        None
    }
}

pub fn normalize_vendor(vendor: Option<&str>) -> Option<String> {
    match vendor? {
        "N/A" => None,
        "Oracle Corporation" => Some("Oracle".to_string()),
        "Azul Systems, Inc." => Some("Azul".to_string()),
        "IBM Corporation" | "International Business Machines Corporation" | "Eclipse OpenJ9" => {
            Some("IBM".to_string())
        }
        "Eclipse Adoptium" => Some("Adoptium".to_string()),
        "Amazon.com Inc." => Some("Amazon".to_string()),
        other => Some(other.to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct JavaInfo {
    pub platform: Platform,
    pub version: String,
    pub vendor: Option<String>,
}

impl JavaInfo {
    pub fn new(platform: Platform, version: impl Into<String>, vendor: Option<String>) -> JavaInfo {
        JavaInfo {
            platform,
            version: version.into(),
            vendor,
        }
    }

    pub fn parsed_major_version(&self) -> Option<u32> {
        parse_major_version(&self.version)
    }

    /// 对应 Java `JavaInfo.fromReleaseFile`。`text` 是 JDK 安装目录下 `release` 文件
    /// 的完整内容。
    ///
    /// ponytail: Java 版对 "OS_NAME 为空字符串 + IMPLEMENTOR 是 'OpenJDK BSD Porting
    /// Team'" 这个组合特判成 FreeBSD；我们的 [`OperatingSystem`] 没有 FreeBSD 变体
    /// （见 `platform.rs` 模块注释——HMCL-rs 不打算支持 FreeBSD），所以这种 release
    /// 文件在这里会落到 `OperatingSystem::parse("")` → `Unknown` → 探测失败，
    /// 而不是被误判成 Linux/Windows 之类。这是有意的收窄，不是遗漏。
    pub fn from_release_file(text: &str) -> Result<JavaInfo, JavaInfoError> {
        let props = load_properties(text);

        let os_name = props.get("OS_NAME").cloned().unwrap_or_default();
        let os_arch = props.get("OS_ARCH").cloned().unwrap_or_default();
        let vendor = props.get("IMPLEMENTOR").cloned();
        let java_version = props.get("JAVA_VERSION").cloned();

        let os = OperatingSystem::parse(&os_name);
        let arch = Architecture::parse(&os_arch);

        if os == OperatingSystem::Unknown {
            return Err(JavaInfoError::UnknownOperatingSystem(os_name));
        }
        if arch == Architecture::Unknown {
            return Err(JavaInfoError::UnknownArchitecture(os_arch));
        }
        let java_version = java_version.ok_or(JavaInfoError::MissingJavaVersion)?;

        Ok(JavaInfo::new(Platform { os, arch }, java_version, vendor))
    }
}

#[derive(Debug, Clone)]
pub struct JavaRuntime {
    pub binary: PathBuf,
    pub info: JavaInfo,
    pub is_managed: bool,
    pub is_jdk: bool,
}

impl JavaRuntime {
    pub fn of(binary: PathBuf, info: JavaInfo, is_managed: bool) -> JavaRuntime {
        let javac_name = if info.platform.os == OperatingSystem::Windows {
            "javac.exe"
        } else {
            "javac"
        };
        let is_jdk = binary.with_file_name(javac_name).is_file();
        JavaRuntime {
            binary,
            info,
            is_managed,
            is_jdk,
        }
    }

    pub fn parsed_version(&self) -> Option<u32> {
        self.info.parsed_major_version()
    }

    pub fn architecture(&self) -> Architecture {
        self.info.platform.arch
    }

    pub fn bits(&self) -> Bits {
        self.architecture().bits()
    }
}

pub fn java_runtime_from_binary(
    binary: PathBuf,
    is_managed: bool,
) -> Result<JavaRuntime, JavaDetectError> {
    let home = binary
        .parent()
        .and_then(|bin| bin.parent())
        .ok_or_else(|| JavaDetectError::NoHomeDirectory(binary.clone()))?;
    let release_path = home.join("release");
    let text = std::fs::read_to_string(&release_path).map_err(|source| {
        JavaDetectError::ReleaseFileUnreadable {
            path: home.to_path_buf(),
            source,
        }
    })?;
    let info = JavaInfo::from_release_file(&text)?;
    Ok(JavaRuntime::of(binary, info, is_managed))
}

/// 极简 Java 探测：`java_override` 给了路径就直接用（多半是 `java-install` 刚装好
/// 的托管 Java），否则退回看 `JAVA_HOME` 和 PATH 上的 `java(.exe)`。真正的注册表/
/// 常见安装目录扫描是本模块文档里明确标注推迟的部分（`JavaManager.java` 那 38KB
/// 的大头），这里先凑合能跑。
pub fn find_a_java(
    java_override: Option<&std::path::Path>,
) -> Result<JavaRuntime, JavaDetectError> {
    if let Some(binary) = java_override {
        return java_runtime_from_binary(binary.to_path_buf(), true);
    }

    let java_name = if cfg!(windows) { "java.exe" } else { "java" };
    let candidates: Vec<PathBuf> = std::env::var("JAVA_HOME")
        .ok()
        .map(|home| PathBuf::from(home).join("bin").join(java_name))
        .into_iter()
        .chain(std::env::var("PATH").ok().into_iter().flat_map(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(java_name))
                .collect::<Vec<_>>()
        }))
        .collect();

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if let Ok(runtime) = java_runtime_from_binary(candidate, false) {
            return Ok(runtime);
        }
    }

    Err(JavaDetectError::NotFound)
}

impl PartialEq for JavaRuntime {
    fn eq(&self, other: &Self) -> bool {
        self.binary == other.binary
    }
}
impl Eq for JavaRuntime {}

impl PartialOrd for JavaRuntime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JavaRuntime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if self.is_managed != other.is_managed {
            return if self.is_managed {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        self.parsed_version()
            .cmp(&other.parsed_version())
            .then_with(|| self.info.version.cmp(&other.info.version))
            .then_with(|| {
                format!("{:?}", self.architecture()).cmp(&format!("{:?}", other.architecture()))
            })
            .then_with(|| self.binary.cmp(&other.binary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMURIN_17_WINDOWS_RELEASE: &str = r#"#
#Thu Oct 19 00:00:00 UTC 2023
IMPLEMENTOR="Eclipse Adoptium"
IMPLEMENTOR_VERSION="Temurin-17.0.9+9"
JAVA_RUNTIME_VERSION="17.0.9+9"
JAVA_VERSION="17.0.9"
JAVA_VERSION_DATE="2023-10-17"
MODULES="java.base,java.compiler"
OS_ARCH="x86_64"
OS_NAME="Windows"
SOURCE=".:git:1234567890ab"
"#;

    const JDK_8_RELEASE: &str = r#"JAVA_VERSION="1.8.0_392"
OS_NAME="Windows"
OS_ARCH="x86_64"
IMPLEMENTOR="Oracle Corporation"
"#;

    #[test]
    fn parses_temurin_17_release_file() {
        let info = JavaInfo::from_release_file(TEMURIN_17_WINDOWS_RELEASE).unwrap();
        assert_eq!(info.version, "17.0.9");
        assert_eq!(info.parsed_major_version(), Some(17));
        assert_eq!(info.platform.os, OperatingSystem::Windows);
        assert_eq!(info.platform.arch, Architecture::X86_64);
        assert_eq!(
            normalize_vendor(info.vendor.as_deref()),
            Some("Adoptium".to_string())
        );
    }

    #[test]
    fn parses_legacy_1_8_version_string() {
        let info = JavaInfo::from_release_file(JDK_8_RELEASE).unwrap();
        assert_eq!(
            info.parsed_major_version(),
            Some(8),
            "1.8.0_392 major version must be 8, not 1"
        );
        assert_eq!(
            normalize_vendor(info.vendor.as_deref()),
            Some("Oracle".to_string())
        );
    }

    #[test]
    fn missing_java_version_is_an_error() {
        let err =
            JavaInfo::from_release_file("OS_NAME=\"Windows\"\nOS_ARCH=\"x86_64\"\n").unwrap_err();
        assert!(matches!(err, JavaInfoError::MissingJavaVersion));
    }

    #[test]
    fn unrecognized_os_is_an_error() {
        let text = "JAVA_VERSION=\"17\"\nOS_NAME=\"PlayStationOS\"\nOS_ARCH=\"x86_64\"\n";
        let err = JavaInfo::from_release_file(text).unwrap_err();
        assert!(matches!(err, JavaInfoError::UnknownOperatingSystem(_)));
    }

    #[test]
    fn quoted_value_escape_sequences_are_decoded() {
        let props = load_properties(r#"KEY="line1\nline2\ttabbed""#);
        assert_eq!(props.get("KEY").unwrap(), "line1\nline2\ttabbed");
    }

    #[test]
    fn comment_lines_and_blank_equals_are_skipped() {
        let props = load_properties("# just a comment\n=no-name\nOK=\"value\"\n");
        assert_eq!(props.len(), 1);
        assert_eq!(props.get("OK").unwrap(), "value");
    }

    #[test]
    fn parse_major_version_handles_both_naming_schemes() {
        assert_eq!(parse_major_version("17.0.9"), Some(17));
        assert_eq!(parse_major_version("1.8.0_392"), Some(8));
        assert_eq!(parse_major_version("21"), Some(21));
        assert_eq!(parse_major_version("not-a-version"), None);
    }

    #[test]
    fn managed_runtime_sorts_before_unmanaged_regardless_of_version() {
        let managed = JavaRuntime {
            binary: PathBuf::from("C:/hmcl/java/8/bin/java.exe"),
            info: JavaInfo::new(
                Platform {
                    os: OperatingSystem::Windows,
                    arch: Architecture::X86_64,
                },
                "8.0.0",
                None,
            ),
            is_managed: true,
            is_jdk: false,
        };
        let unmanaged_newer = JavaRuntime {
            binary: PathBuf::from("C:/Program Files/Java/jdk-21/bin/java.exe"),
            info: JavaInfo::new(
                Platform {
                    os: OperatingSystem::Windows,
                    arch: Architecture::X86_64,
                },
                "21.0.0",
                None,
            ),
            is_managed: false,
            is_jdk: true,
        };
        assert!(
            managed < unmanaged_newer,
            "HMCL-managed runtimes sort first even if their version is lower"
        );
    }
}

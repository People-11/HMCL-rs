use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::de::Error as DeError;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::platform::{Architecture, OperatingSystem, Platform};

pub const DEFAULT_LIBRARY_URL: &str = "https://libraries.minecraft.net/";
pub const DEFAULT_VERSION_DOWNLOAD_URL: &str = "https://bmclapi2.bangbang93.com/versions/";
pub const DEFAULT_INDEX_URL: &str = "https://launchermeta.mojang.com/v1/packages/";

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("malformed artifact descriptor: {0}")]
    MalformedArtifact(String),
    #[error("version not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    pub platform: Platform,
    pub os_version: &'a str,
}

impl<'a> Env<'a> {
    pub const fn current(os_version: &'a str) -> Env<'a> {
        Env {
            platform: Platform::CURRENT,
            os_version,
        }
    }
}

fn empty_features() -> &'static HashMap<String, bool> {
    static EMPTY: LazyLock<HashMap<String, bool>> = LazyLock::new(HashMap::new);
    &EMPTY
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Artifact {
    pub group: String,
    pub name: String,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl Artifact {
    pub fn new(
        group: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Artifact {
        Artifact {
            group: group.into(),
            name: name.into(),
            version: version.into(),
            classifier: None,
            extension: "jar".to_string(),
        }
    }

    pub fn from_descriptor(descriptor: &str) -> Result<Artifact, VersionError> {
        let parts: Vec<&str> = descriptor.splitn(4, ':').collect();
        if parts.len() != 3 && parts.len() != 4 {
            return Err(VersionError::MalformedArtifact(descriptor.to_string()));
        }

        let last_idx = parts.len() - 1;
        let mut last = parts[last_idx].to_string();
        let mut extension: Option<String> = None;
        if last.matches('@').count() == 1 {
            let (base, ext) = last.split_once('@').unwrap();
            extension = Some(ext.to_string());
            last = base.to_string();
        } else if last.matches('@').count() > 1 {
            return Err(VersionError::MalformedArtifact(descriptor.to_string()));
        }

        let (version, classifier) = if last_idx == 3 {
            (parts[2].to_string(), Some(last))
        } else {
            (last, None)
        };

        Ok(Artifact {
            group: parts[0].replace('\\', "/"),
            name: parts[1].to_string(),
            version,
            classifier,
            extension: extension.unwrap_or_else(|| "jar".to_string()),
        })
    }

    pub fn file_name(&self) -> String {
        let mut s = format!("{}-{}", self.name, self.version);
        if let Some(c) = &self.classifier {
            s.push('-');
            s.push_str(c);
        }
        s.push('.');
        s.push_str(&self.extension);
        s
    }

    pub fn path(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.name,
            self.version,
            self.file_name()
        )
    }

    pub fn with_classifier(&self, classifier: Option<String>) -> Artifact {
        Artifact {
            classifier,
            ..self.clone()
        }
    }
}

impl fmt::Display for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.name, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        if self.extension != "jar" {
            write!(f, "@{}", self.extension)?;
        }
        Ok(())
    }
}

impl Serialize for Artifact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Artifact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Artifact, D::Error> {
        let s = String::deserialize(deserializer)?;
        Artifact::from_descriptor(&s).map_err(DeError::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    Disallow,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct OsRestriction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

fn full_match_regex(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{pattern})$"))
}

impl OsRestriction {
    /// 对应 Java `OSRestriction.allow()`。正则编译失败时视为"不限制"（和 Java
    /// `Lang.test` 吞异常后走 false 分支的效果一致）。
    ///
    /// ponytail: 不支持 FreeBSD（HMCL-rs 只面向 Windows/Linux/macOS 桌面用户），
    /// 所以 Java 里 "Linux-or-BSD" 的特判在这里收窄成单纯的 `Linux == Linux`。
    pub fn allows(&self, env: Env) -> bool {
        if let Some(name) = &self.name {
            let parts: Vec<&str> = name.splitn(3, '-').collect();
            if parts.len() == 2 {
                let os = OperatingSystem::parse(parts[0]);
                let arch = Architecture::parse(parts[1]);
                if os != OperatingSystem::Unknown && arch != Architecture::Unknown {
                    if os != env.platform.os {
                        return false;
                    }
                    if arch != env.platform.arch {
                        return false;
                    }
                    return true;
                }
            }
        }

        let os = OperatingSystem::parse(self.name.as_deref().unwrap_or(""));
        if os != OperatingSystem::Unknown && os != env.platform.os {
            return false;
        }

        if let Some(version) = &self.version {
            if let Ok(re) = full_match_regex(version) {
                if !re.is_match(env.os_version) {
                    return false;
                }
            }
        }

        if let Some(arch) = &self.arch {
            return match full_match_regex(arch) {
                Ok(re) => re.is_match(env.platform.arch.checked_name()),
                Err(_) => true,
            };
        }

        true
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompatibilityRule {
    pub action: RuleAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsRestriction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<HashMap<String, bool>>,
}

impl CompatibilityRule {
    pub fn applied_action(&self, env: Env, features: &HashMap<String, bool>) -> Option<RuleAction> {
        if let Some(os) = &self.os {
            if !os.allows(env) {
                return None;
            }
        }
        if let Some(rule_features) = &self.features {
            for (k, v) in rule_features {
                if features.get(k) != Some(v) {
                    return None;
                }
            }
        }
        Some(self.action)
    }

    pub fn applies_to_current_environment(
        rules: &[CompatibilityRule],
        env: Env,
        features: &HashMap<String, bool>,
    ) -> bool {
        if rules.is_empty() {
            return true;
        }
        let mut action = RuleAction::Disallow;
        for rule in rules {
            if let Some(a) = rule.applied_action(env, features) {
                action = a;
            }
        }
        action == RuleAction::Allow
    }
}

#[derive(Debug, Clone)]
pub enum Argument {
    Plain(String),
    Ruled {
        rules: Vec<CompatibilityRule>,
        value: Vec<String>,
    },
}

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{[^}]*\}").unwrap());

pub fn substitute(s: &str, keys: &HashMap<String, String>) -> String {
    PLACEHOLDER
        .replace_all(s, |caps: &regex::Captures| {
            let token = &caps[0];
            keys.get(token)
                .cloned()
                .unwrap_or_else(|| token.to_string())
        })
        .into_owned()
}

impl Argument {
    pub fn to_strings(
        &self,
        keys: &HashMap<String, String>,
        env: Env,
        features: &HashMap<String, bool>,
    ) -> Vec<String> {
        match self {
            Argument::Plain(s) => vec![substitute(s, keys)],
            Argument::Ruled { rules, value } => {
                if CompatibilityRule::applies_to_current_environment(rules, env, features) {
                    value.iter().map(|v| substitute(v, keys)).collect()
                } else {
                    Vec::new()
                }
            }
        }
    }
}

impl Serialize for Argument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Argument::Plain(s) => serializer.serialize_str(s),
            Argument::Ruled { rules, value } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("rules", rules)?;
                map.serialize_entry("value", value)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Argument, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(Argument::Plain(s)),
            serde_json::Value::Object(mut obj) => {
                let rules = match obj.remove("rules") {
                    Some(v) => serde_json::from_value(v).map_err(DeError::custom)?,
                    None => Vec::new(),
                };
                let value_elem = obj
                    .remove("value")
                    .or_else(|| obj.remove("values"))
                    .ok_or_else(|| DeError::custom("argument object missing 'value'/'values'"))?;
                let value = match value_elem {
                    serde_json::Value::String(s) => vec![s],
                    other => serde_json::from_value(other).map_err(DeError::custom)?,
                };
                Ok(Argument::Ruled { rules, value })
            }
            other => Err(DeError::custom(format!(
                "unexpected argument JSON shape: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Arguments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<Vec<Argument>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jvm: Option<Vec<Argument>>,
}

fn merge_opt_vec<T: Clone>(a: &Option<Vec<T>>, b: &Option<Vec<T>>) -> Option<Vec<T>> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let mut result = Vec::new();
    if let Some(a) = a {
        result.extend(a.iter().cloned());
    }
    if let Some(b) = b {
        result.extend(b.iter().cloned());
    }
    Some(result)
}

impl Arguments {
    pub fn merge(a: Option<&Arguments>, b: Option<&Arguments>) -> Option<Arguments> {
        match (a, b) {
            (None, None) => None,
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (Some(a), Some(b)) => Some(Arguments {
                game: merge_opt_vec(&a.game, &b.game),
                jvm: merge_opt_vec(&a.jvm, &b.jvm),
            }),
        }
    }

    pub fn parse_arguments(
        args: &[Argument],
        keys: &HashMap<String, String>,
        env: Env,
        features: &HashMap<String, bool>,
    ) -> Vec<String> {
        args.iter()
            .flat_map(|a| a.to_strings(keys, env, features))
            .collect()
    }

    /// 对应 Java `Arguments.parseStringArguments`：给老版本 `minecraftArguments`
    /// 字符串分词后的结果、以及用户自定义的额外参数用的——单纯做占位符替换，
    /// 不涉及 rule 匹配（这些字符串本来就没有条件）。
    pub fn parse_string_arguments(args: &[String], keys: &HashMap<String, String>) -> Vec<String> {
        args.iter().map(|s| substitute(s, keys)).collect()
    }
}

/// 对应 Java `Arguments.DEFAULT_JVM_ARGUMENTS`：老版本 `minecraftArguments` 格式的
/// 版本 json 没有自带 jvm 参数列表，启动时用这份兜底。
pub fn default_jvm_arguments() -> Vec<Argument> {
    vec![
        Argument::Ruled {
            rules: vec![CompatibilityRule {
                action: RuleAction::Allow,
                os: Some(OsRestriction {
                    name: Some(OperatingSystem::Windows.mojang_name().to_string()),
                    version: None,
                    arch: None,
                }),
                features: None,
            }],
            value: vec!["-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump".to_string()],
        },
        Argument::Ruled {
            rules: vec![CompatibilityRule {
                action: RuleAction::Allow,
                os: Some(OsRestriction {
                    name: Some(OperatingSystem::Windows.mojang_name().to_string()),
                    version: Some(r"^10\.".to_string()),
                    arch: None,
                }),
                features: None,
            }],
            value: vec!["-Dos.name=Windows 10".to_string(), "-Dos.version=10.0".to_string()],
        },
        Argument::Plain("-Djava.library.path=${natives_directory}".to_string()),
        Argument::Plain("-Dminecraft.launcher.brand=${launcher_name}".to_string()),
        Argument::Plain("-Dminecraft.launcher.version=${launcher_version}".to_string()),
        Argument::Plain("-cp".to_string()),
        Argument::Plain("${classpath}".to_string()),
    ]
}

pub fn default_game_arguments() -> Vec<Argument> {
    vec![Argument::Ruled {
        rules: vec![CompatibilityRule {
            action: RuleAction::Allow,
            os: None,
            features: Some(HashMap::from([("has_custom_resolution".to_string(), true)])),
        }],
        value: vec![
            "--width".to_string(),
            "${resolution_width}".to_string(),
            "--height".to_string(),
            "${resolution_height}".to_string(),
        ],
    }]
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct DownloadInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: u64,
}

impl DownloadInfo {
    pub fn new(url: impl Into<String>) -> DownloadInfo {
        DownloadInfo {
            url: Some(url.into()),
            sha1: None,
            size: 0,
        }
    }

    /// `"invalid"` 是 HMCL 生态里对"这个 sha1 不可信, 别拿来校验"的哨兵值。
    pub fn checksum(&self) -> Option<&str> {
        match self.sha1.as_deref() {
            Some("invalid") | None => None,
            Some(s) => Some(s),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct IdDownloadInfo {
    #[serde(default)]
    pub id: String,
    #[serde(flatten)]
    pub download: DownloadInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AssetIndexInfo {
    #[serde(flatten)]
    pub base: IdDownloadInfo,
    #[serde(default, rename = "totalSize")]
    pub total_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LoggingInfo {
    pub file: IdDownloadInfo,
    pub argument: String,
    #[serde(rename = "type")]
    pub log_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadType {
    Client,
    Server,
    WindowsServer,
    ClientMappings,
    ServerMappings,
}

/// 注意 JSON 里的写法（`snake_case`）和 [`ReleaseType::id`] 返回的写法
/// （`kebab-case`）是两套不同的字符串，不能混：
/// - JSON（Mojang 版本清单和 version.json 的 `"type"` 字段）用的是 `"old_beta"`，
///   对应 Java 侧的 `LowerCaseEnumTypeAdapterFactory`——它把枚举的 `name()`
///   （`OLD_BETA`）整个转小写来匹配，得到的就是下划线形式。
/// - `id()` 返回的 `"old-beta"` 只用在命令行的 `${version_type}` 占位符上，
///   对应 Java 侧 `ReleaseType.getId()`，跟 JSON 无关。
///
/// 这里原来写成 `rename_all = "kebab-case"` 是个真 bug：`old_beta`/`old_alpha`
/// 两个值会匹配不上而被 `#[serde(other)]` 悄悄吞成 `Unknown`，下载页的"远古版"
/// 分类会全军覆没。其余变体都是单个单词，两种写法一样，所以之前没暴露出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType {
    Release,
    Snapshot,
    Modified,
    OldBeta,
    OldAlpha,
    Pending,
    Unobfuscated,
    #[serde(other)]
    Unknown,
}

impl ReleaseType {
    pub fn id(self) -> &'static str {
        match self {
            ReleaseType::Release => "release",
            ReleaseType::Snapshot => "snapshot",
            ReleaseType::Modified => "modified",
            ReleaseType::OldBeta => "old-beta",
            ReleaseType::OldAlpha => "old-alpha",
            ReleaseType::Pending => "pending",
            ReleaseType::Unobfuscated => "unobfuscated",
            ReleaseType::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GameJavaVersion {
    pub component: String,
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ExtractRules {
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl ExtractRules {
    pub fn should_extract(&self, path: &str) -> bool {
        !self.exclude.iter().any(|e| path.starts_with(e.as_str()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LibraryDownloadInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(flatten)]
    pub download: DownloadInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LibrariesDownloadInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<LibraryDownloadInfo>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub classifiers: HashMap<String, LibraryDownloadInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Library {
    #[serde(rename = "name")]
    pub artifact: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<LibrariesDownloadInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<ExtractRules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natives: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<CompatibilityRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksums: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "MMC-hint")]
    pub hint: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "filename",
        alias = "MMC-filename"
    )]
    pub file_name: Option<String>,
}

impl Library {
    fn possible_native_descriptors(platform: Platform) -> Vec<String> {
        let keys = [
            "",
            platform.arch.checked_name(),
            platform.arch.bits().as_str(),
        ];
        let variants = ["", "native", "natives"];
        let mut out = Vec::with_capacity(keys.len() * variants.len());
        for key in keys {
            for variant in variants {
                let mut s = String::new();
                if !variant.is_empty() {
                    s.push_str(variant);
                    s.push('-');
                }
                s.push_str(platform.os.mojang_name());
                if !key.is_empty() {
                    s.push('-');
                    s.push_str(key);
                }
                out.push(s);
            }
        }
        out
    }

    pub fn classifier(&self, platform: Platform) -> Option<String> {
        if let Some(c) = &self.artifact.classifier {
            return Some(c.clone());
        }
        if let Some(natives) = &self.natives {
            for nd in Self::possible_native_descriptors(platform) {
                if let Some(v) = natives.get(&nd) {
                    return Some(v.replace("${arch}", platform.arch.bits().as_str()));
                }
            }
            None
        } else if let Some(downloads) = &self.downloads {
            Self::possible_native_descriptors(platform)
                .into_iter()
                .find(|nd| downloads.classifiers.contains_key(nd))
        } else {
            None
        }
    }

    pub fn extract(&self) -> ExtractRules {
        self.extract.clone().unwrap_or_default()
    }

    pub fn applies_to(&self, env: Env) -> bool {
        CompatibilityRule::applies_to_current_environment(&self.rules, env, empty_features())
    }

    pub fn is_native(&self, env: Env) -> bool {
        if !self.applies_to(env) {
            return false;
        }
        if self.natives.is_some() {
            return true;
        }
        self.downloads
            .as_ref()
            .map(|d| d.classifiers.keys().any(|k| k.starts_with("native")))
            .unwrap_or(false)
    }

    pub fn raw_download_info(&self, env: Env) -> Option<&LibraryDownloadInfo> {
        let downloads = self.downloads.as_ref()?;
        if self.is_native(env) {
            downloads.classifiers.get(&self.classifier(env.platform)?)
        } else {
            downloads.artifact.as_ref()
        }
    }

    pub fn path(&self, env: Env) -> String {
        if let Some(p) = self.raw_download_info(env).and_then(|r| r.path.clone()) {
            return p;
        }
        self.artifact
            .with_classifier(self.classifier(env.platform))
            .path()
    }

    fn compute_url(&self, raw: Option<&LibraryDownloadInfo>, path: &str) -> String {
        if let Some(url) = raw.and_then(|r| r.download.url.clone()) {
            return url;
        }
        let repo = self
            .url
            .clone()
            .unwrap_or_else(|| DEFAULT_LIBRARY_URL.to_string());
        let repo = if repo.ends_with('/') {
            repo
        } else {
            format!("{repo}/")
        };
        format!("{repo}{path}")
    }

    pub fn has_download_url(&self, env: Env) -> bool {
        match self.raw_download_info(env) {
            Some(raw) => raw.download.url.is_some(),
            None => self.url.is_some(),
        }
    }

    pub fn download(&self, env: Env) -> LibraryDownloadInfo {
        let path = self.path(env);
        let raw = self.raw_download_info(env);
        LibraryDownloadInfo {
            path: Some(path.clone()),
            download: DownloadInfo {
                url: Some(self.compute_url(raw, &path)),
                sha1: raw.and_then(|r| r.download.sha1.clone()),
                size: raw.map(|r| r.download.size).unwrap_or(0),
            },
        }
    }

    pub fn is(&self, group: &str, name: &str) -> bool {
        self.artifact.group == group && self.artifact.name == name
    }

    /// 从裸 Maven 坐标构造一个没有 url/downloads/rules 的最简 Library——安装器
    /// (Forge 旧版/OptiFine) 从压缩包里把文件直接抠出来放到本地时用得上，这类库
    /// 本来就不需要下载信息。
    pub fn from_artifact(artifact: Artifact) -> Library {
        Library {
            artifact,
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Version {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "minecraftArguments"
    )]
    pub minecraft_arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "mainClass")]
    pub main_class: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "inheritsFrom"
    )]
    pub inherits_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jar: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "assetIndex"
    )]
    pub asset_index: Option<AssetIndexInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "complianceLevel"
    )]
    pub compliance_level: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "javaVersion"
    )]
    pub java_version: Option<GameJavaVersion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<Library>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "compatibilityRules"
    )]
    pub compatibility_rules: Vec<CompatibilityRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<HashMap<DownloadType, DownloadInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<HashMap<DownloadType, LoggingInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub release_type: Option<ReleaseType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "releaseTime"
    )]
    pub release_time: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "minimumLauncherVersion"
    )]
    pub minimum_launcher_version: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patches: Option<Vec<Version>>,
}

impl Version {
    /// 对应 Java `Version.PRIORITY_MC`/`PRIORITY_LOADER`：patch 优先级数字，越大
    /// 越晚合并（合并顺序见 `resolve_inner` 里的 `sort_by_key(priority_or_default)`），
    /// 也就越能覆盖前面的 `mainClass` 等字段。安装器（Fabric/Forge/...）的 patch
    /// 统一用 `PRIORITY_LOADER`。
    pub const PRIORITY_MC: i32 = 0;
    pub const PRIORITY_LOADER: i32 = 30000;

    pub fn new(id: impl Into<String>) -> Version {
        Version {
            id: id.into(),
            hidden: Some(false),
            root: Some(true),
            ..Default::default()
        }
    }

    pub fn priority_or_default(&self) -> i32 {
        self.priority.unwrap_or(i32::MIN)
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden.unwrap_or(false)
    }

    pub fn is_root(&self) -> bool {
        self.root.unwrap_or(false)
    }

    pub fn minimum_launcher_version_or_default(&self) -> i32 {
        self.minimum_launcher_version.unwrap_or(0)
    }

    pub fn applies_to_current_environment(&self, env: Env) -> bool {
        CompatibilityRule::applies_to_current_environment(
            &self.compatibility_rules,
            env,
            empty_features(),
        )
    }

    pub fn client_download_info(&self) -> DownloadInfo {
        if let Some(d) = self
            .downloads
            .as_ref()
            .and_then(|d| d.get(&DownloadType::Client))
        {
            return d.clone();
        }
        let jar_name = self.jar.clone().unwrap_or_else(|| self.id.clone());
        DownloadInfo::new(format!(
            "{DEFAULT_VERSION_DOWNLOAD_URL}{jar_name}/{jar_name}.jar"
        ))
    }

    /// 对应 Java `Version.getAssetIndex()`：老版本没有显式 `assetIndex` 字段时，
    /// 按已知的 legacy assets id 查表拼 URL。
    pub fn asset_index(&self) -> AssetIndexInfo {
        if let Some(ai) = &self.asset_index {
            return ai.clone();
        }
        let requested = self.assets.as_deref().unwrap_or("legacy");
        let (assets_id, hash) = match requested {
            "1.8" => ("1.8", "f6ad102bcaa53b1a58358f16e376d548d44933ec"),
            "14w31a" => ("14w31a", "10a2a0e75b03cfb5a7196abbdf43b54f7fa61deb"),
            "14w25a" => ("14w25a", "32ff354a3be1c4dd83027111e6d79ee4d701d2c0"),
            "1.7.4" => ("1.7.4", "545510a60f526b9aa8a38f9c0bc7a74235d21675"),
            "1.7.10" => ("1.7.10", "1863782e33ce7b584fc45b037325a1964e095d3e"),
            "1.7.3" => ("1.7.3", "f6cf726f4747128d13887010c2cbc44ba83504d9"),
            "pre-1.6" => ("pre-1.6", "3d8e55480977e32acd9844e545177e69a52f594b"),
            _ => ("legacy", "770572e819335b6c0a053f8378ad88eda189fc14"),
        };
        AssetIndexInfo {
            base: IdDownloadInfo {
                id: assets_id.to_string(),
                download: DownloadInfo::new(format!("{DEFAULT_INDEX_URL}{hash}/{assets_id}.json")),
            },
            total_size: 0,
        }
    }

    fn to_patch(&self) -> Version {
        let mut v = self.clone();
        v.patches = None;
        v.hidden = Some(true);
        v.id = format!("resolved.{}", self.id);
        v
    }

    /// 对应 Java `Version.merge(Version parent, boolean isPatch)`。
    /// 注意几处不对称的地方（照抄, 不是笔误）：
    /// - `compliance_level` 不回落到父版本;
    /// - `hidden` 直接用子版本自己的值, 不回落;
    /// - `downloads`/`logging` 是整表替换, 不是逐 key 合并, 而且是按 `None`/`Some`
    ///   回落(不是按"是否为空"回落)——LiteLoader 会显式设一个空的 `logging` 表
    ///   来压制 vanilla 的 log4j XML 配置, 这时候必须保留"空表"本身, 不能因为它是空的
    ///   就当成没提供而回落到父版本。
    fn merge(&self, parent: &Version, is_patch: bool) -> Version {
        let patches = if is_patch {
            parent.patches.clone()
        } else {
            let mut merged = parent.patches.clone().unwrap_or_default();
            merged.push(self.to_patch());
            if let Some(self_patches) = &self.patches {
                merged.extend(self_patches.iter().cloned());
            }
            Some(merged)
        };

        let mut libraries = self.libraries.clone();
        libraries.extend(parent.libraries.iter().cloned());

        let mut compatibility_rules = parent.compatibility_rules.clone();
        compatibility_rules.extend(self.compatibility_rules.iter().cloned());

        Version {
            id: self.id.clone(),
            version: None,
            priority: None,
            minecraft_arguments: self
                .minecraft_arguments
                .clone()
                .or_else(|| parent.minecraft_arguments.clone()),
            arguments: Arguments::merge(parent.arguments.as_ref(), self.arguments.as_ref()),
            main_class: self
                .main_class
                .clone()
                .or_else(|| parent.main_class.clone()),
            inherits_from: None,
            jar: self.jar.clone().or_else(|| parent.jar.clone()),
            asset_index: self
                .asset_index
                .clone()
                .or_else(|| parent.asset_index.clone()),
            assets: self.assets.clone().or_else(|| parent.assets.clone()),
            compliance_level: self.compliance_level,
            java_version: self
                .java_version
                .clone()
                .or_else(|| parent.java_version.clone()),
            libraries,
            compatibility_rules,
            downloads: self.downloads.clone().or_else(|| parent.downloads.clone()),
            logging: self.logging.clone().or_else(|| parent.logging.clone()),
            release_type: self.release_type.or(parent.release_type),
            time: self.time.clone().or_else(|| parent.time.clone()),
            release_time: self
                .release_time
                .clone()
                .or_else(|| parent.release_time.clone()),
            minimum_launcher_version: match (
                self.minimum_launcher_version,
                parent.minimum_launcher_version,
            ) {
                (None, b) => b,
                (a, None) => a,
                (Some(a), Some(b)) => Some(a.max(b)),
            },
            hidden: self.hidden,
            root: Some(true),
            patches,
        }
    }

    pub fn resolve(&self, provider: &impl VersionProvider) -> Result<Version, VersionError> {
        self.resolve_inner(provider, &mut HashSet::new())
    }

    fn resolve_inner(
        &self,
        provider: &impl VersionProvider,
        resolved_so_far: &mut HashSet<String>,
    ) -> Result<Version, VersionError> {
        let mut this_version: Version;

        if self.inherits_from.is_none() {
            this_version = if self.is_root() {
                match &self.patches {
                    Some(patches) => {
                        let mut v = Version::new(self.id.clone());
                        v.patches = Some(patches.clone());
                        v
                    }
                    None => self.clone(),
                }
            } else {
                self.clone()
            };
            this_version.jar = Some(self.jar.clone().unwrap_or_else(|| self.id.clone()));
        } else {
            let parent_id = self.inherits_from.clone().unwrap();
            if !resolved_so_far.insert(self.id.clone()) {
                tracing::warn!(resolved = ?resolved_so_far, "Found circular dependency versions");
                this_version = self.clone();
                if this_version.jar.is_none() {
                    this_version.jar = Some(self.id.clone());
                }
            } else {
                let parent = provider
                    .version(&parent_id)
                    .ok_or_else(|| VersionError::NotFound(parent_id.clone()))?;
                let resolved_parent = parent.resolve_inner(provider, resolved_so_far)?;
                this_version = self.merge(&resolved_parent, false);
            }
        }

        match &self.patches {
            None => Ok(this_version),
            Some(patches) if patches.is_empty() => {
                this_version.id = self.id.clone();
                Ok(this_version)
            }
            Some(patches) => {
                let mut sorted = patches.clone();
                sorted.sort_by_key(|p| p.priority_or_default());
                for mut patch in sorted {
                    patch.jar = None;
                    this_version = patch.merge(&this_version, true);
                }
                this_version.id = self.id.clone();
                Ok(this_version)
            }
        }
    }
}

pub trait VersionProvider {
    fn version(&self, id: &str) -> Option<Version>;
}

impl VersionProvider for HashMap<String, Version> {
    fn version(&self, id: &str) -> Option<Version> {
        self.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Version {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"))
    }

    #[test]
    fn parses_real_1_20_1_and_resolves_jar() {
        let v = fixture("1.20.1.json");
        assert_eq!(v.id, "1.20.1");
        assert_eq!(
            v.main_class.as_deref(),
            Some("net.minecraft.client.main.Main")
        );
        assert!(v.libraries.len() > 10);
        assert_eq!(v.java_version.as_ref().unwrap().major_version, 17);

        let resolved = v
            .resolve(&HashMap::new())
            .expect("standalone version resolves without a provider");
        assert_eq!(resolved.jar.as_deref(), Some("1.20.1"));
    }

    #[test]
    fn round_trips_through_json() {
        let v = fixture("1.20.1.json");
        let json = serde_json::to_string(&v).unwrap();
        let v2: Version = serde_json::from_str(&json).unwrap();
        assert_eq!(v.id, v2.id);
        assert_eq!(v.libraries.len(), v2.libraries.len());
        assert_eq!(v.main_class, v2.main_class);
        assert_eq!(
            v.downloads
                .as_ref()
                .and_then(|d| d.get(&DownloadType::Client)),
            v2.downloads
                .as_ref()
                .and_then(|d| d.get(&DownloadType::Client))
        );
        assert_eq!(v.asset_index, v2.asset_index);
    }

    #[test]
    fn ruled_argument_with_single_string_value_is_normalized_to_vec() {
        let v = fixture("1.20.1.json");
        let jvm = v.arguments.as_ref().unwrap().jvm.as_ref().unwrap();
        let ruled_single = jvm.iter().find_map(|a| match a {
            Argument::Ruled { value, .. }
                if value.len() == 1 && value[0].starts_with("-XX:HeapDumpPath") =>
            {
                Some(value.clone())
            }
            _ => None,
        });
        assert_eq!(ruled_single, Some(vec!["-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump".to_string()]));
    }

    #[test]
    fn legacy_asset_index_fallback_matches_known_hashes() {
        let with_assets = |assets: &str| Version {
            assets: Some(assets.to_string()),
            ..Default::default()
        };

        let pre16 = with_assets("pre-1.6").asset_index();
        assert_eq!(pre16.base.id, "pre-1.6");
        assert_eq!(
            pre16.base.download.checksum(),
            None,
            "fallback path never sets sha1, matching Java"
        );
        assert_eq!(
            pre16.base.download.url.as_deref(),
            Some("https://launchermeta.mojang.com/v1/packages/3d8e55480977e32acd9844e545177e69a52f594b/pre-1.6.json")
        );

        assert!(with_assets("1.8")
            .asset_index()
            .base
            .download
            .url
            .unwrap()
            .contains("f6ad102bcaa53b1a58358f16e376d548d44933ec"));
        assert_eq!(
            Version::default().asset_index().base.id,
            "legacy",
            "no `assets` field at all falls back to legacy"
        );
        assert_eq!(
            with_assets("unknown-id").asset_index().base.id,
            "legacy",
            "unrecognized id also falls back to legacy"
        );
    }

    #[test]
    fn native_library_classifier_resolves_per_platform() {
        let v = fixture("1.12.2.json");
        let lwjgl_platform = v
            .libraries
            .iter()
            .find(|l| l.is("org.lwjgl.lwjgl", "lwjgl-platform"))
            .expect("1.12.2 must ship lwjgl-platform with natives");

        let env = Env::current("");
        let win = Env {
            platform: Platform::WINDOWS_X64,
            os_version: "",
        };
        let linux = Env {
            platform: Platform::LINUX_X64,
            os_version: "",
        };
        let _ = env;

        assert_eq!(
            lwjgl_platform.classifier(win.platform),
            Some("natives-windows".to_string())
        );
        assert_eq!(
            lwjgl_platform.classifier(linux.platform),
            Some("natives-linux".to_string())
        );
        assert!(lwjgl_platform.is_native(win));
        assert!(lwjgl_platform.is_native(linux));

        let download = lwjgl_platform.download(win);
        assert!(download
            .download
            .url
            .unwrap()
            .ends_with("lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar"));
    }

    #[test]
    fn library_os_rule_excludes_macos() {
        let v = fixture("1.12.2.json");
        let lib = v
            .libraries
            .iter()
            .find(|l| l.is("org.lwjgl.lwjgl", "lwjgl-platform"))
            .unwrap();
        assert!(lib.applies_to(Env {
            platform: Platform::WINDOWS_X64,
            os_version: ""
        }));
        assert!(!lib.applies_to(Env {
            platform: Platform::MACOS_ARM64,
            os_version: ""
        }));
    }

    #[test]
    fn arch_rule_x86_does_not_partial_match_x86_64() {
        let rule = OsRestriction {
            name: None,
            version: None,
            arch: Some("x86".to_string()),
        };
        assert!(
            !rule.allows(Env {
                platform: Platform::WINDOWS_X64,
                os_version: ""
            }),
            "\"x86\" 规则不能匹配 x86_64"
        );
        let x86_platform = Platform {
            os: crate::platform::OperatingSystem::Windows,
            arch: crate::platform::Architecture::X86,
        };
        assert!(
            rule.allows(Env {
                platform: x86_platform,
                os_version: ""
            }),
            "\"x86\" 规则必须匹配纯 32 位 x86"
        );
    }

    #[test]
    fn legacy_minecraft_arguments_are_tokenizable_placeholders() {
        let v = fixture("1.12.2.json");
        let args = v
            .minecraft_arguments
            .as_ref()
            .expect("1.12.2 uses the old minecraftArguments string format");
        assert!(args.contains("${auth_player_name}"));
        assert!(
            v.arguments.is_none(),
            "old-format versions have no structured `arguments` block"
        );
    }

    #[test]
    fn resolves_inherits_from_chain_with_priority_ordered_patches() {
        let mut provider = HashMap::new();

        let mut vanilla = Version::new("1.20.1");
        vanilla.main_class = Some("net.minecraft.client.main.Main".to_string());
        vanilla.libraries = vec![Library {
            artifact: Artifact::new("com.mojang", "vanilla-lib", "1.0"),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }];
        provider.insert("1.20.1".to_string(), vanilla);

        let mut low_priority_patch = Version::new("low-priority-loader");
        low_priority_patch.priority = Some(1);
        low_priority_patch.main_class = Some("should.be.overridden.ByHigherPriority".to_string());

        let mut high_priority_patch = Version::new("fabric-loader");
        high_priority_patch.priority = Some(30000);
        high_priority_patch.main_class =
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string());
        high_priority_patch.libraries = vec![Library {
            artifact: Artifact::new("net.fabricmc", "fabric-loader", "0.15.0"),
            url: None,
            downloads: None,
            extract: None,
            natives: None,
            rules: Vec::new(),
            checksums: None,
            hint: None,
            file_name: None,
        }];

        let mut instance = Version::new("1.20.1-fabric");
        instance.inherits_from = Some("1.20.1".to_string());
        instance.patches = Some(vec![high_priority_patch, low_priority_patch]); // 故意乱序, resolve 要自己排序

        let resolved = instance.resolve(&provider).expect("chain resolves");

        assert_eq!(resolved.id, "1.20.1-fabric");
        assert_eq!(
            resolved.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient"),
            "higher-priority patch (30000) must win over lower-priority patch (1) regardless of list order"
        );
        assert_eq!(
            resolved.libraries.len(),
            2,
            "must carry both the patch's own library and the inherited vanilla library"
        );
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("net.fabricmc", "fabric-loader")));
        assert!(resolved
            .libraries
            .iter()
            .any(|l| l.is("com.mojang", "vanilla-lib")));
    }

    #[test]
    fn patch_can_explicitly_clear_parent_logging_with_an_empty_map_not_just_omit_it() {
        let mut vanilla = Version::new("1.6.4");
        vanilla.logging = Some(HashMap::from([(
            DownloadType::Client,
            LoggingInfo {
                file: IdDownloadInfo {
                    id: "client-1.6.4.xml".to_string(),
                    download: DownloadInfo::new("https://example.com/client-1.6.4.xml"),
                },
                argument: "-Dlog4j.configurationFile=${path}".to_string(),
                log_type: "log4j2-xml".to_string(),
            },
        )]));

        let mut liteloader_patch = Version::new("liteloader");
        liteloader_patch.priority = Some(60000);
        liteloader_patch.logging = Some(HashMap::new()); // 显式清空, 不是没设置

        let mut instance = Version::new("1.6.4-liteloader");
        instance.inherits_from = Some("1.6.4".to_string());
        instance.patches = Some(vec![liteloader_patch]);

        let mut provider = HashMap::new();
        provider.insert("1.6.4".to_string(), vanilla);

        let resolved = instance.resolve(&provider).expect("chain resolves");
        assert_eq!(resolved.logging, Some(HashMap::new()), "an explicitly-empty logging map from a patch must suppress the parent's logging config, not fall back to it");
    }

    #[test]
    fn missing_parent_version_is_reported_as_not_found() {
        let mut instance = Version::new("orphan");
        instance.inherits_from = Some("does-not-exist".to_string());
        let err = instance
            .resolve(&HashMap::<String, Version>::new())
            .unwrap_err();
        assert!(matches!(err, VersionError::NotFound(id) if id == "does-not-exist"));
    }

    #[test]
    fn artifact_descriptor_round_trip_with_classifier_and_extension() {
        let a = Artifact::from_descriptor("net.fabricmc:fabric-loader:0.15.0").unwrap();
        assert_eq!(a.to_string(), "net.fabricmc:fabric-loader:0.15.0");
        assert_eq!(
            a.path(),
            "net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"
        );

        let b = Artifact::from_descriptor("org.lwjgl:lwjgl:3.3.1:natives-windows").unwrap();
        assert_eq!(b.classifier.as_deref(), Some("natives-windows"));
        assert_eq!(b.to_string(), "org.lwjgl:lwjgl:3.3.1:natives-windows");

        let c = Artifact::from_descriptor("org.example:tool:1.0@zip").unwrap();
        assert_eq!(c.extension, "zip");
        assert_eq!(c.version, "1.0");
        assert_eq!(c.to_string(), "org.example:tool:1.0@zip");
    }

    #[test]
    fn windows_10_jvm_arg_rule_uses_java_full_match_semantics() {
        let args = default_jvm_arguments();
        let win10_rule = args
            .iter()
            .find(|a| matches!(a, Argument::Ruled { value, .. } if value.iter().any(|v| v.contains("Windows 10"))))
            .unwrap();

        let keys = HashMap::new();
        let features = HashMap::new();

        let on_real_win10 = win10_rule.to_strings(
            &keys,
            Env {
                platform: Platform::WINDOWS_X64,
                os_version: "10.0.19045.2965",
            },
            &features,
        );
        assert!(
            on_real_win10.is_empty(),
            "真实的四段版本号字符串对不上 \"^10\\.\" 的全串匹配"
        );

        let on_shortest_realistic = win10_rule.to_strings(
            &keys,
            Env {
                platform: Platform::WINDOWS_X64,
                os_version: "10.0",
            },
            &features,
        );
        assert!(
            on_shortest_realistic.is_empty(),
            "哪怕是最短的 major.minor 形式 \"10.0\" 也多出一个字符, 全串匹配不上"
        );

        let on_exact_pattern = win10_rule.to_strings(
            &keys,
            Env {
                platform: Platform::WINDOWS_X64,
                os_version: "10.",
            },
            &features,
        );
        assert_eq!(
            on_exact_pattern,
            vec!["-Dos.name=Windows 10", "-Dos.version=10.0"]
        );

        let on_win7 = win10_rule.to_strings(
            &keys,
            Env {
                platform: Platform::WINDOWS_X64,
                os_version: "6.1.7601",
            },
            &features,
        );
        assert!(on_win7.is_empty());

        let on_linux = win10_rule.to_strings(
            &keys,
            Env {
                platform: Platform::LINUX_X64,
                os_version: "10.",
            },
            &features,
        );
        assert!(
            on_linux.is_empty(),
            "the rule is Windows-only regardless of the version string"
        );
    }

    #[test]
    fn placeholder_substitution_leaves_unknown_tokens_untouched() {
        let mut keys = HashMap::new();
        keys.insert("${classpath}".to_string(), "a.jar;b.jar".to_string());
        assert_eq!(substitute("-cp ${classpath}", &keys), "-cp a.jar;b.jar");
        assert_eq!(substitute("${unknown_token}", &keys), "${unknown_token}");
    }
}

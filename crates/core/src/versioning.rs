use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EarlyAccessType {
    Snapshot,
    PreRelease,
    ReleaseCandidate,
    Ga,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameVersionNumber {
    major: u32,
    minor: u32,
    patch: u32,
    era: EarlyAccessType,
    era_version: Vec<u32>,
    unobfuscated: bool,
}

const MINIMUM_YEAR_MAJOR_VERSION: u32 = 26;

static VERSION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<major>1|[1-9][0-9]+)\.(?P<minor>[0-9]+)(?:\.(?P<patch>[0-9]+))?(?P<suffix>.*)$",
    )
    .unwrap()
});

fn parse_numeric_dotted(s: &str) -> Option<Vec<u32>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split('.').map(|p| p.parse().ok()).collect()
}

fn compare_numeric_dotted(a: &[u32], b: &[u32]) -> Ordering {
    for i in 0..a.len().max(b.len()) {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

impl GameVersionNumber {
    /// 对应 Java `Release.parse`：只搬运"标准"分隔符形式（`-snapshot-N`、`-preN`/
    /// `-pre-N`、`-rcN`/`-rc-N`、`_unobfuscated`）。不搬那几个历史遗留的、给人类可读
    /// 文本字段兼容用的带空格变体（`" Pre-Release "` 之类）——现在的 Mojang 版本
    /// 清单不会再产出那种格式。
    pub fn parse(value: &str) -> Option<GameVersionNumber> {
        let caps = VERSION_PATTERN.captures(value)?;

        let major: u32 = caps["major"].parse().ok()?;
        if major != 1 && major < MINIMUM_YEAR_MAJOR_VERSION {
            return None;
        }
        let minor: u32 = caps["minor"].parse().ok()?;
        let patch: u32 = match caps.name("patch") {
            Some(m) => m.as_str().parse().ok()?,
            None => 0,
        };

        let mut suffix = &caps["suffix"];
        let mut unobfuscated = false;
        if let Some(stripped) = suffix.strip_suffix("_unobfuscated") {
            suffix = stripped;
            unobfuscated = true;
        }

        let (era, era_version) = if suffix.is_empty() {
            (EarlyAccessType::Ga, Vec::new())
        } else if let Some(rest) = suffix.strip_prefix("-snapshot-") {
            (EarlyAccessType::Snapshot, parse_numeric_dotted(rest)?)
        } else if let Some(rest) = suffix.strip_prefix("-pre-") {
            (EarlyAccessType::PreRelease, parse_numeric_dotted(rest)?)
        } else if let Some(rest) = suffix.strip_prefix("-pre") {
            (EarlyAccessType::PreRelease, parse_numeric_dotted(rest)?)
        } else if let Some(rest) = suffix.strip_prefix("-rc-") {
            (
                EarlyAccessType::ReleaseCandidate,
                parse_numeric_dotted(rest)?,
            )
        } else if let Some(rest) = suffix.strip_prefix("-rc") {
            (
                EarlyAccessType::ReleaseCandidate,
                parse_numeric_dotted(rest)?,
            )
        } else {
            return None;
        };

        Some(GameVersionNumber {
            major,
            minor,
            patch,
            era,
            era_version,
            unobfuscated,
        })
    }

    /// 对应 Java `GameVersionNumber.compare(String, String)`。两边只要有一个认不出来
    /// 就返回 `None`——调用方应该把它当成"不知道，跳过版本相关的特殊处理"，不要
    /// 塞一个默认值硬比。
    pub fn compare(a: &str, b: &str) -> Option<Ordering> {
        Some(GameVersionNumber::parse(a)?.cmp(&GameVersionNumber::parse(b)?))
    }
}

impl Ord for GameVersionNumber {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| self.era.cmp(&other.era))
            .then_with(|| compare_numeric_dotted(&self.era_version, &other.era_version))
            .then_with(|| self.unobfuscated.cmp(&other.unobfuscated))
    }
}

impl PartialOrd for GameVersionNumber {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_major_minor_patch_numerically_not_lexically() {
        assert_eq!(
            GameVersionNumber::compare("1.9", "1.10"),
            Some(Ordering::Less),
            "1.10 是 1.10, 不是 1.1"
        );
        assert_eq!(
            GameVersionNumber::compare("1.20.1", "1.19"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            GameVersionNumber::compare("1.7", "1.7"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            GameVersionNumber::compare("1.6.4", "1.7"),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn release_sorts_after_its_own_snapshots_pre_releases_and_rcs() {
        assert_eq!(
            GameVersionNumber::compare("26.2", "26.2-snapshot-2"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            GameVersionNumber::compare("26.2-rc-1", "26.2"),
            Some(Ordering::Less)
        );
        assert_eq!(
            GameVersionNumber::compare("26.2-pre1", "26.2-rc1"),
            Some(Ordering::Less),
            "pre-release < release-candidate"
        );
        assert_eq!(
            GameVersionNumber::compare("26.2-snapshot-1", "26.2-snapshot-2"),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn dash_and_dash_dash_suffix_forms_both_parse() {
        assert_eq!(
            GameVersionNumber::compare("26.2-rc1", "26.2-rc-1"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            GameVersionNumber::compare("1.16-pre1", "1.16-pre-1"),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn the_four_actual_thresholds_used_by_command_line_generation() {
        assert_eq!(
            GameVersionNumber::compare("1.18.2", "1.19"),
            Some(Ordering::Less)
        ); // natives ASCII 路径 workaround
        assert_eq!(
            GameVersionNumber::compare("1.20.4", "1.19"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            GameVersionNumber::compare("1.6.4", "1.7"),
            Some(Ordering::Less)
        ); // log4j 是否启用
        assert_eq!(
            GameVersionNumber::compare("1.12.2", "1.7"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            GameVersionNumber::compare("1.20.1", "26.2-snapshot-2"),
            Some(Ordering::Less)
        ); // graphicsBackend 参数
        assert_eq!(
            GameVersionNumber::compare("26.2", "26.2-snapshot-2"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn old_alpha_beta_and_weekly_snapshot_codenames_are_unrecognized_not_guessed() {
        assert_eq!(GameVersionNumber::parse("a1.0.5"), None);
        assert_eq!(GameVersionNumber::parse("b1.7.3"), None);
        assert_eq!(GameVersionNumber::parse("14w25a"), None);
        assert_eq!(
            GameVersionNumber::compare("14w25a", "1.19"),
            None,
            "调用方看到 None 就该跳过该版本相关的特殊处理"
        );
    }

    #[test]
    fn single_digit_non_one_major_is_rejected_by_design() {
        assert_eq!(GameVersionNumber::parse("2.0"), None);
        assert_eq!(GameVersionNumber::parse("9.9"), None);
        assert!(GameVersionNumber::parse("26.0").is_some());
    }

    #[test]
    fn version_digits_are_ascii_like_javas_default_regex_mode() {
        assert!(GameVersionNumber::parse("1.20.1").is_some());
        assert_eq!(GameVersionNumber::parse("1.٢٠.1"), None);
    }

    #[test]
    fn unobfuscated_suffix_is_recognized_and_sorts_after_the_normal_release() {
        let normal = GameVersionNumber::parse("1.20.1").unwrap();
        let unobf = GameVersionNumber::parse("1.20.1_unobfuscated").unwrap();
        assert!(unobf > normal);
    }
}

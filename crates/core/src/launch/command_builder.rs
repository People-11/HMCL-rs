use std::sync::LazyLock;

use regex::Regex;

static UNSTABLE_KV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-XX:(?P<key>[a-zA-Z0-9]+)=(?P<value>.*)$").unwrap());
static UNSTABLE_BOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-XX:(?P<sign>[+\-])(?P<key>[a-zA-Z0-9]+)$").unwrap());

#[derive(Debug, Default)]
pub struct CommandBuilder {
    items: Vec<(String, bool)>,
    external: Vec<String>,
}

impl CommandBuilder {
    pub fn new() -> CommandBuilder {
        CommandBuilder::default()
    }

    fn all_existing_args(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .map(|(a, _)| a.as_str())
            .chain(self.external.iter().map(|s| s.as_str()))
    }

    pub fn add(&mut self, arg: impl Into<String>) -> &mut Self {
        self.items.push((arg.into(), true));
        self
    }

    pub fn add_all<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self.items.push((arg.into(), true));
        }
        self
    }

    pub fn add_without_parsing(&mut self, arg: impl Into<String>) -> &mut Self {
        self.items.push((arg.into(), false));
        self
    }

    pub fn add_all_without_parsing<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self.items.push((arg.into(), false));
        }
        self
    }

    pub fn add_all_without_parsing_and_read_external<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.add_all_without_parsing(args)
    }

    fn add_default_impl(&mut self, opt: &str, value: &str, parse: bool) -> Option<String> {
        if let Some(existing) = self.all_existing_args().find(|a| a.starts_with(opt)) {
            let existing = existing.to_string();
            tracing::info!(opt, value, overridden_by = %existing, "default option suppressed");
            return Some(existing);
        }
        self.items.push((format!("{opt}{value}"), parse));
        None
    }

    pub fn add_default(&mut self, opt: &str, value: &str) -> Option<String> {
        self.add_default_impl(opt, value, true)
    }

    fn add_unstable_default_bool_impl(
        &mut self,
        opt: &str,
        value: bool,
        parse: bool,
    ) -> Option<String> {
        for (arg, _) in &self.items {
            if let Some(caps) = UNSTABLE_BOOL.captures(arg) {
                if &caps["key"] == opt {
                    return Some(arg.clone());
                }
            }
        }
        for arg in &self.external {
            if let Some(caps) = UNSTABLE_BOOL.captures(arg) {
                if &caps["key"] == opt {
                    return Some(arg.clone());
                }
            }
        }
        self.items
            .push((format!("-XX:{}{opt}", if value { "+" } else { "-" }), parse));
        None
    }

    pub fn add_unstable_default(&mut self, opt: &str, value: bool) -> Option<String> {
        self.add_unstable_default_bool_impl(opt, value, true)
    }

    fn add_unstable_default_kv_impl(
        &mut self,
        opt: &str,
        value: &str,
        parse: bool,
    ) -> Option<String> {
        for (arg, _) in &self.items {
            if let Some(caps) = UNSTABLE_KV.captures(arg) {
                if &caps["key"] == opt {
                    return Some(arg.clone());
                }
            }
        }
        for arg in &self.external {
            if let Some(caps) = UNSTABLE_KV.captures(arg) {
                if &caps["key"] == opt {
                    return Some(arg.clone());
                }
            }
        }
        self.items.push((format!("-XX:{opt}={value}"), parse));
        None
    }

    pub fn add_unstable_default_kv(&mut self, opt: &str, value: &str) -> Option<String> {
        self.add_unstable_default_kv_impl(opt, value, true)
    }

    fn add_all_default_impl(&mut self, args: &[String], parse: bool) {
        for arg in args {
            if arg.starts_with("-D") {
                if let Some(eq_idx) = arg.find('=') {
                    let opt = arg[..=eq_idx].to_string();
                    let value = arg[eq_idx + 1..].to_string();
                    self.add_default_impl(&opt, &value, parse);
                } else {
                    let opt = format!("{arg}=");
                    let suppressor = self
                        .all_existing_args()
                        .find(|a| a.starts_with(opt.as_str()) || *a == arg.as_str());
                    if let Some(existing) = suppressor {
                        if existing != arg.as_str() {
                            tracing::info!(default = %arg, overridden_by = existing, "default option suppressed");
                        }
                        continue;
                    }
                    self.items.push((arg.clone(), parse));
                }
                continue;
            }

            if arg.starts_with("-XX:") {
                if let Some(caps) = UNSTABLE_KV.captures(arg) {
                    let key = caps["key"].to_string();
                    let value = caps["value"].to_string();
                    self.add_unstable_default_kv_impl(&key, &value, parse);
                    continue;
                }
                if let Some(caps) = UNSTABLE_BOOL.captures(arg) {
                    let key = caps["key"].to_string();
                    let sign_is_plus = &caps["sign"] == "+";
                    self.add_unstable_default_bool_impl(&key, sign_is_plus, parse);
                    continue;
                }
            }

            if arg.starts_with("-X") {
                const PREFIXES: [&str; 4] = ["-Xmx", "-Xms", "-Xmn", "-Xss"];
                if let Some(prefix) = PREFIXES.iter().find(|p| arg.starts_with(**p)) {
                    let value = arg[prefix.len()..].to_string();
                    self.add_default_impl(prefix, &value, parse);
                    continue;
                }
            }

            if self.all_existing_args().all(|a| a != arg.as_str()) {
                self.items.push((arg.clone(), parse));
            }
        }
    }

    pub fn add_all_default(&mut self, args: &[String]) {
        self.add_all_default_impl(args, true);
    }

    pub fn add_all_default_without_parsing(&mut self, args: &[String]) {
        self.add_all_default_impl(args, false);
    }

    pub fn remove_if(&mut self, pred: impl Fn(&str) -> bool) -> bool {
        let before = self.items.len();
        self.items.retain(|(a, _)| !pred(a));
        before != self.items.len()
    }

    pub fn none_match(&self, pred: impl Fn(&str) -> bool) -> bool {
        self.items.iter().all(|(a, _)| !pred(a))
    }

    pub fn as_list(&self) -> Vec<String> {
        self.items.iter().map(|(a, _)| a.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn render(&self) -> String {
        self.items
            .iter()
            .map(|(arg, parse)| {
                if *parse {
                    to_batch_string_literal(arg)
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn escape(mut s: String, chars: &[char]) -> String {
    for &ch in chars {
        s = s.replace(ch, &format!("\\{ch}"));
    }
    s
}

pub fn to_batch_string_literal(s: &str) -> String {
    const NEEDS_QUOTING: &str = " \t\"^&<>|";
    if s.chars().any(|c| NEEDS_QUOTING.contains(c)) {
        format!("\"{}\"", escape(s.to_string(), &['\\', '"']))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_supplied_memory_flag_suppresses_the_default() {
        let mut cb = CommandBuilder::new();
        cb.add_all_without_parsing(["-Xmx8192m".to_string()]);
        let overridden = cb.add_default("-Xmx", "4096m");
        assert_eq!(
            overridden,
            Some("-Xmx8192m".to_string()),
            "must report what suppressed it"
        );
        assert_eq!(
            cb.as_list(),
            vec!["-Xmx8192m".to_string()],
            "the default must not have been appended"
        );
    }

    #[test]
    fn add_default_appends_when_nothing_conflicts() {
        let mut cb = CommandBuilder::new();
        let result = cb.add_default("-Xmx", "4096m");
        assert_eq!(result, None);
        assert_eq!(cb.as_list(), vec!["-Xmx4096m".to_string()]);
    }

    #[test]
    fn unstable_default_bool_is_suppressed_by_existing_matching_key_regardless_of_sign() {
        let mut cb = CommandBuilder::new();
        cb.add("-XX:-UseG1GC"); // 用户显式关掉了 G1GC
        let overridden = cb.add_unstable_default("UseG1GC", true); // 默认值是"开"
        assert_eq!(overridden, Some("-XX:-UseG1GC".to_string()));
        assert_eq!(
            cb.as_list(),
            vec!["-XX:-UseG1GC".to_string()],
            "user's -XX:-UseG1GC must win, not get a second +UseG1GC appended"
        );
    }

    #[test]
    fn unstable_default_kv_appends_in_key_value_form() {
        let mut cb = CommandBuilder::new();
        cb.add_unstable_default_kv("MaxGCPauseMillis", "50");
        assert_eq!(cb.as_list(), vec!["-XX:MaxGCPauseMillis=50".to_string()]);
    }

    #[test]
    fn add_all_default_routes_dash_d_properties_through_add_default() {
        let mut cb = CommandBuilder::new();
        cb.add("-Dfile.encoding=GBK"); // 用户已经指定了编码
        cb.add_all_default(&["-Dfile.encoding=UTF-8".to_string()]);
        assert_eq!(
            cb.as_list(),
            vec!["-Dfile.encoding=GBK".to_string()],
            "user's -D property wins"
        );
    }

    #[test]
    fn add_all_default_routes_xmx_style_flags_through_add_default() {
        let mut cb = CommandBuilder::new();
        cb.add("-Xmx8192m");
        cb.add_all_default(&["-Xmx4096m".to_string()]);
        assert_eq!(cb.as_list(), vec!["-Xmx8192m".to_string()]);
    }

    #[test]
    fn add_all_default_routes_xx_flags_through_unstable_default() {
        let mut cb = CommandBuilder::new();
        cb.add("-XX:+UseG1GC");
        cb.add_all_default(&["-XX:-UseG1GC".to_string()]); // 默认想关, 但用户已经开了
        assert_eq!(cb.as_list(), vec!["-XX:+UseG1GC".to_string()]);
    }

    #[test]
    fn add_all_default_plain_flag_is_deduplicated_by_exact_match_only() {
        let mut cb = CommandBuilder::new();
        cb.add("--enable-native-access=ALL-UNNAMED");
        cb.add_all_default(&[
            "--enable-native-access=ALL-UNNAMED".to_string(),
            "--something-else".to_string(),
        ]);
        assert_eq!(
            cb.as_list(),
            vec![
                "--enable-native-access=ALL-UNNAMED".to_string(),
                "--something-else".to_string()
            ],
            "exact duplicate dropped, unrelated default flag still added"
        );
    }

    #[test]
    fn batch_quoting_wraps_only_when_special_chars_present() {
        assert_eq!(to_batch_string_literal("plain"), "plain");
        assert_eq!(to_batch_string_literal("has space"), "\"has space\"");
        assert_eq!(to_batch_string_literal(r#"say "hi""#), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn render_joins_with_quoting_applied_only_to_parsed_items() {
        let mut cb = CommandBuilder::new();
        cb.add("java");
        cb.add("has space");
        cb.add_without_parsing("--raw=already quoted by caller");
        assert_eq!(
            cb.render(),
            "java \"has space\" --raw=already quoted by caller"
        );
    }
}

pub struct DownloadProvider {
    /// (前缀, 替换成的前缀)，命中第一条就替换并停止，未命中原样返回。
    /// 顺序即优先级，和 Java `BMCLAPIDownloadProvider.replacement` 字段序一致。
    replacement: Vec<(String, String)>,
    fallback_replacement: Vec<(String, String)>,
    version_manifest_url: String,
    asset_object_base: String,
    concurrency: usize,
    fallback: Option<Box<DownloadProvider>>,
}

fn inject(table: &[(String, String)], base_url: &str) -> Option<String> {
    table
        .iter()
        .find(|(prefix, _)| base_url.starts_with(prefix.as_str()))
        .map(|(prefix, replacement)| format!("{replacement}{}", &base_url[prefix.len()..]))
}

impl DownloadProvider {
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn mojang() -> DownloadProvider {
        DownloadProvider {
            replacement: Vec::new(),
            fallback_replacement: Vec::new(),
            // v2 而不是 HMCL-java 用的 v1：两者内容一样, 但 v2 的每个条目多带一个
            // `sha1`, 那是官方唯一一处能用来校验 version.json 的哈希（见
            // `install::download_version_json`）。实测 BMCLAPI 也镜像了 v2, 而且
            // 转发的 version.json 与官方逐字节一致, 所以镜像路径同样能通过校验。
            version_manifest_url: "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
                .to_string(),
            asset_object_base: "https://resources.download.minecraft.net/".to_string(),
            concurrency: 6,
            fallback: None,
        }
    }

    /// BMCLAPI 及其兼容镜像。`api_root` 形如 `"https://bmclapi2.bangbang93.com"`（不带尾部斜杠）。
    pub fn bmclapi(api_root: impl Into<String>) -> DownloadProvider {
        let api_root = api_root.into();
        let s = |suffix: &str| format!("{api_root}{suffix}");
        DownloadProvider {
            replacement: vec![
                (
                    "https://bmclapi2.bangbang93.com".to_string(),
                    api_root.clone(),
                ),
                (
                    "https://launchermeta.mojang.com".to_string(),
                    api_root.clone(),
                ),
                (
                    "https://piston-meta.mojang.com".to_string(),
                    api_root.clone(),
                ),
                (
                    "https://piston-data.mojang.com".to_string(),
                    api_root.clone(),
                ),
                ("https://launcher.mojang.com".to_string(), api_root.clone()),
                (
                    "https://libraries.minecraft.net".to_string(),
                    s("/libraries"),
                ),
                (
                    "http://files.minecraftforge.net/maven".to_string(),
                    s("/maven"),
                ),
                (
                    "https://files.minecraftforge.net/maven".to_string(),
                    s("/maven"),
                ),
                ("https://maven.minecraftforge.net".to_string(), s("/maven")),
                (
                    "https://maven.neoforged.net/releases/".to_string(),
                    s("/maven/"),
                ),
                (
                    "http://dl.liteloader.com/versions/versions.json".to_string(),
                    s("/maven/com/mumfrey/liteloader/versions.json"),
                ),
                ("http://dl.liteloader.com/versions".to_string(), s("/maven")),
                ("https://meta.fabricmc.net".to_string(), s("/fabric-meta")),
                ("https://maven.fabricmc.net".to_string(), s("/maven")),
                (
                    "https://authlib-injector.yushi.moe".to_string(),
                    s("/mirrors/authlib-injector"),
                ),
                (
                    "https://repo1.maven.org/maven2".to_string(),
                    "https://mirrors.cloud.tencent.com/nexus/repository/maven-public".to_string(),
                ),
                (
                    "https://repo.maven.apache.org/maven2".to_string(),
                    "https://mirrors.cloud.tencent.com/nexus/repository/maven-public".to_string(),
                ),
                (
                    "https://hmcl.glavo.site/metadata/cleanroom".to_string(),
                    "https://alist.8mi.tech/d/mirror/HMCL-Metadata/Auto/cleanroom".to_string(),
                ),
                (
                    "https://hmcl.glavo.site/metadata/fmllibs".to_string(),
                    "https://alist.8mi.tech/d/mirror/HMCL-Metadata/Auto/fmllibs".to_string(),
                ),
                (
                    "https://zkitefly.github.io/unlisted-versions-of-minecraft".to_string(),
                    "https://alist.8mi.tech/d/mirror/unlisted-versions-of-minecraft/Auto"
                        .to_string(),
                ),
            ],
            fallback_replacement: vec![
                (
                    "https://api.modrinth.com".to_string(),
                    "https://mod.mcimirror.top/modrinth".to_string(),
                ),
                (
                    "https://cdn.modrinth.com".to_string(),
                    "https://mod.mcimirror.top".to_string(),
                ),
                (
                    "https://api.curseforge.com".to_string(),
                    "https://mod.mcimirror.top/curseforge".to_string(),
                ),
                (
                    "https://edge.forgecdn.net".to_string(),
                    "https://mod.mcimirror.top".to_string(),
                ),
            ],
            version_manifest_url: s("/mc/game/version_manifest_v2.json"),
            asset_object_base: s("/assets/"),
            concurrency: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                * 2,
            fallback: None,
        }
        .with_min_concurrency(6)
    }

    pub fn auto(prefer_mirror: bool) -> DownloadProvider {
        let (mut primary, fallback) = if prefer_mirror {
            (
                Self::bmclapi(crate::download::DEFAULT_BMCLAPI_API_ROOT),
                Self::mojang(),
            )
        } else {
            (
                Self::mojang(),
                Self::bmclapi(crate::download::DEFAULT_BMCLAPI_API_ROOT),
            )
        };
        primary.fallback = Some(Box::new(fallback));
        primary
    }

    fn with_min_concurrency(mut self, min: usize) -> DownloadProvider {
        self.concurrency = self.concurrency.max(min);
        self
    }

    pub fn version_manifest_candidates(&self) -> Vec<String> {
        let mut candidates = vec![self.version_manifest_url.clone()];
        if let Some(fallback) = &self.fallback {
            for candidate in fallback.version_manifest_candidates() {
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        candidates
    }

    pub fn asset_object_candidates(&self, asset_object_location: &str) -> Vec<String> {
        let mut candidates = vec![format!("{}{asset_object_location}", self.asset_object_base)];
        if let Some(fallback) = &self.fallback {
            for candidate in fallback.asset_object_candidates(asset_object_location) {
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        candidates
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// 对应 Java `injectURL`：替换命中就返回替换后的，否则原样返回。
    pub fn inject_url(&self, base_url: &str) -> String {
        inject(&self.replacement, base_url).unwrap_or_else(|| base_url.to_string())
    }

    pub fn inject_url_candidates(&self, base_url: &str) -> Vec<String> {
        let mut candidates = if let Some(injected) = inject(&self.replacement, base_url) {
            vec![injected]
        } else {
            match inject(&self.fallback_replacement, base_url) {
                Some(fallback) => vec![base_url.to_string(), fallback],
                None => vec![base_url.to_string()],
            }
        };
        if let Some(fallback) = &self.fallback {
            for candidate in fallback.inject_url_candidates(base_url) {
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mojang_provider_never_rewrites() {
        let p = DownloadProvider::mojang();
        let url = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
        assert_eq!(p.inject_url(url), url);
        assert_eq!(p.inject_url_candidates(url), vec![url.to_string()]);
    }

    #[test]
    fn auto_provider_uses_hmcl_region_order_with_fallback() {
        let url = "https://libraries.minecraft.net/foo.jar";
        assert_eq!(
            DownloadProvider::auto(false).inject_url_candidates(url),
            vec![
                url.to_string(),
                "https://bmclapi2.bangbang93.com/libraries/foo.jar".to_string(),
            ]
        );
        assert_eq!(
            DownloadProvider::auto(true).inject_url_candidates(url),
            vec![
                "https://bmclapi2.bangbang93.com/libraries/foo.jar".to_string(),
                url.to_string(),
            ]
        );
        assert!(
            DownloadProvider::auto(false).version_manifest_candidates()[0]
                .starts_with("https://piston-meta.mojang.com")
        );
        assert!(
            DownloadProvider::auto(true).version_manifest_candidates()[0]
                .starts_with("https://bmclapi2.bangbang93.com")
        );
    }

    #[test]
    fn bmclapi_rewrites_known_prefixes() {
        let p = DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com");
        assert_eq!(
            p.inject_url("https://libraries.minecraft.net/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"),
            "https://bmclapi2.bangbang93.com/libraries/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"
        );
        assert_eq!(
            p.inject_url("https://piston-data.mojang.com/v1/objects/abc/client.jar"),
            "https://bmclapi2.bangbang93.com/v1/objects/abc/client.jar"
        );
        assert_eq!(
            p.inject_url("https://example.com/foo"),
            "https://example.com/foo"
        );
    }

    #[test]
    fn bmclapi_candidates_prefer_original_for_fallback_only_hosts() {
        let p = DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com");
        let candidates = p.inject_url_candidates("https://api.modrinth.com/v2/project/foo");
        assert_eq!(
            candidates,
            vec![
                "https://api.modrinth.com/v2/project/foo".to_string(),
                "https://mod.mcimirror.top/modrinth/v2/project/foo".to_string(),
            ]
        );

        let libs = p.inject_url_candidates("https://libraries.minecraft.net/foo.jar");
        assert_eq!(libs.len(), 1);
        assert!(libs[0].starts_with("https://bmclapi2.bangbang93.com/libraries/"));
    }

    #[test]
    fn bmclapi_concurrency_has_a_floor_of_six() {
        assert!(DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com").concurrency() >= 6);
    }

    #[test]
    fn settings_can_override_download_concurrency() {
        assert_eq!(
            DownloadProvider::mojang()
                .with_concurrency(32)
                .concurrency(),
            32
        );
        assert_eq!(
            DownloadProvider::mojang().with_concurrency(0).concurrency(),
            1
        );
    }

    #[test]
    fn asset_object_url_uses_dedicated_endpoint_not_the_prefix_table() {
        let mojang = DownloadProvider::mojang();
        assert_eq!(
            mojang.asset_object_candidates("ab/abcdef1234567890"),
            vec!["https://resources.download.minecraft.net/ab/abcdef1234567890".to_string()]
        );

        let bmclapi = DownloadProvider::bmclapi("https://bmclapi2.bangbang93.com");
        assert_eq!(
            bmclapi.asset_object_candidates("ab/abcdef1234567890"),
            vec!["https://bmclapi2.bangbang93.com/assets/ab/abcdef1234567890".to_string()]
        );
    }
}

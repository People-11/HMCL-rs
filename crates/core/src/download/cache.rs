use std::path::{Path, PathBuf};

pub struct CacheRepository {
    root: PathBuf,
}

impl CacheRepository {
    pub fn new(root: impl Into<PathBuf>) -> CacheRepository {
        CacheRepository { root: root.into() }
    }

    fn cached_path(&self, sha1: &str) -> PathBuf {
        let sha1 = sha1.to_lowercase();
        let prefix = &sha1[..sha1.len().min(2)];
        self.root.join("files").join(prefix).join(&sha1)
    }

    pub async fn contains(&self, sha1: &str) -> bool {
        tokio::fs::metadata(self.cached_path(sha1)).await.is_ok()
    }

    /// 把已经下载好、已校验过 sha1 的文件登记进缓存。硬链接优先（同一卷零拷贝），
    /// 跨卷硬链接失败时退化成复制。
    pub async fn put(&self, sha1: &str, src: &Path) -> std::io::Result<()> {
        let cached = self.cached_path(sha1);
        if tokio::fs::metadata(&cached).await.is_ok() {
            return Ok(());
        }
        if let Some(parent) = cached.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if tokio::fs::hard_link(src, &cached).await.is_err() {
            tokio::fs::copy(src, &cached).await?;
        }
        Ok(())
    }

    pub async fn link_from_cache(&self, sha1: &str, dest: &Path) -> std::io::Result<bool> {
        let cached = self.cached_path(sha1);
        if tokio::fs::metadata(&cached).await.is_err() {
            return Ok(false);
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if tokio::fs::hard_link(&cached, dest).await.is_err() {
            tokio::fs::copy(&cached, dest).await?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test-cache")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn put_then_link_from_cache_reproduces_content() {
        let dir = tmp_dir("put_then_link");
        let cache = CacheRepository::new(dir.join("cache"));
        let src = dir.join("source.jar");
        tokio::fs::write(&src, b"library bytes").await.unwrap();

        let sha1 = crate::download::fetch::sha1_hex(b"library bytes");
        assert!(!cache.contains(&sha1).await);

        cache.put(&sha1, &src).await.unwrap();
        assert!(cache.contains(&sha1).await);

        let dest = dir.join("instance-a").join("libs").join("library.jar");
        let linked = cache.link_from_cache(&sha1, &dest).await.unwrap();
        assert!(linked);
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"library bytes");
    }

    #[tokio::test]
    async fn miss_returns_false_without_touching_dest() {
        let dir = tmp_dir("miss");
        let cache = CacheRepository::new(dir.join("cache"));
        let dest = dir.join("out.jar");
        let hit = cache
            .link_from_cache("0000000000000000000000000000000000000000", &dest)
            .await
            .unwrap();
        assert!(!hit);
        assert!(!dest.exists());
    }
}

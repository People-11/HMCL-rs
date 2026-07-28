use std::path::{Path, PathBuf};

use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::cache::CacheRepository;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("no candidate URLs were provided")]
    NoCandidates,
    #[error("all {attempted} candidate URL(s) failed")]
    AllCandidatesFailed {
        attempted: usize,
        #[source]
        last: Box<FetchError>,
    },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch: expected sha1 {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
}

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    LoaderStarted {
        name: String,
    },
    LoaderFinished,
    StageStarted {
        stage: InstallStage,
        total: usize,
    },
    TaskDone {
        stage: InstallStage,
    },
    /// 正在真实下载的某个文件收到了新的一块数据。`chunk_bytes` 是这一块的大小,
    /// 不是累计值；`total_bytes` 来自清单里的预期大小，缺失时是 `None`。
    /// 缓存命中/已存在的文件不会有这个事件(根本没有字节流过网络)。
    Bytes {
        path: PathBuf,
        chunk_bytes: u64,
        total_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    Libraries,
    AssetObjects,
    ModpackArchive,
    ModpackFiles,
}

pub type ProgressSink = tokio::sync::mpsc::UnboundedSender<ProgressEvent>;

#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

impl Expected {
    pub fn sha1(sha1: impl Into<String>) -> Expected {
        Expected {
            sha1: Some(sha1.into()),
            size: None,
        }
    }

    pub fn sha1_and_size(sha1: impl Into<String>, size: u64) -> Expected {
        Expected {
            sha1: Some(sha1.into()),
            size: Some(size),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    to_hex(&Sha1::digest(bytes))
}

/// 流式计算文件 SHA-1，避免校验大型 JAR/资源时把整个文件读入内存。
pub async fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

const RETRIES_PER_CANDIDATE: u32 = 3;

/// 文件已经存在且满足校验条件时跳过网络请求。没有任何校验信息(既没 sha1 也没 size)
/// 时只要文件存在就当作已满足——这是我们能做到的极限，调用方应该尽量总是提供 sha1。
async fn already_satisfied(dest: &Path, expected: &Expected) -> bool {
    let Ok(metadata) = tokio::fs::metadata(dest).await else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if let Some(expected_size) = expected.size {
        if expected_size != 0 && metadata.len() != expected_size {
            return false;
        }
    }
    if let Some(expected_sha1) = &expected.sha1 {
        let Ok(actual) = sha1_file(dest).await else {
            return false;
        };
        return actual.eq_ignore_ascii_case(expected_sha1);
    }
    true
}

fn part_file(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// 下载并校验一次。`on_chunk` 每收到一块数据就回调一次（参数是这一块的字节数,
/// 不是累计值）——`fetch_to_file`/`fetch_to_file_with_progress` 都是薄壳套这个
/// 函数，前者传一个空实现，不重复实现下载/重试/校验逻辑。
async fn try_fetch_once_with_progress(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: &Expected,
    on_chunk: &mut impl FnMut(u64),
) -> Result<(), FetchError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = part_file(dest);
    let response = client.get(url).send().await?.error_for_status()?;

    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut hasher = Sha1::new();
    let mut total: u64 = 0;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        total += chunk.len() as u64;
        on_chunk(chunk.len() as u64);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    if let Some(expected_size) = expected.size {
        if expected_size != 0 && total != expected_size {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(FetchError::SizeMismatch {
                expected: expected_size,
                actual: total,
            });
        }
    }
    if let Some(expected_sha1) = &expected.sha1 {
        let actual = to_hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha1) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(FetchError::ChecksumMismatch {
                expected: expected_sha1.clone(),
                actual,
            });
        }
    }

    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// 依次尝试 `candidates` 里的每个 URL（每个最多重试 `RETRIES_PER_CANDIDATE` 次），
/// 下载到 `dest`。`dest` 已存在且通过校验时直接返回，不发请求。
pub async fn fetch_to_file(
    client: &reqwest::Client,
    candidates: &[String],
    dest: &Path,
    expected: &Expected,
) -> Result<(), FetchError> {
    fetch_to_file_with_progress(client, candidates, dest, expected, |_| {}).await
}

pub async fn fetch_to_file_with_progress(
    client: &reqwest::Client,
    candidates: &[String],
    dest: &Path,
    expected: &Expected,
    mut on_chunk: impl FnMut(u64),
) -> Result<(), FetchError> {
    if candidates.is_empty() {
        return Err(FetchError::NoCandidates);
    }

    if already_satisfied(dest, expected).await {
        return Ok(());
    }

    let mut last_err: Option<FetchError> = None;
    for url in candidates {
        for attempt in 0..RETRIES_PER_CANDIDATE {
            match try_fetch_once_with_progress(client, url, dest, expected, &mut on_chunk).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(url, attempt, error = %e, "download attempt failed");
                    last_err = Some(e);
                }
            }
        }
    }

    Err(FetchError::AllCandidatesFailed {
        attempted: candidates.len(),
        last: Box::new(last_err.expect("loop ran at least once since candidates is non-empty")),
    })
}

pub async fn fetch_to_file_cached(
    client: &reqwest::Client,
    cache: &CacheRepository,
    candidates: &[String],
    dest: &Path,
    expected: &Expected,
) -> Result<(), FetchError> {
    fetch_to_file_cached_with_progress(client, cache, candidates, dest, expected, |_| {}).await
}

/// 跟 [`fetch_to_file_cached`] 完全一样，多一个"每收到一块数据就回调一次"的
/// 钩子——缓存命中/文件已存在这两条走了就直接返回的路径不会触发回调，因为根本
/// 没有字节流过网络。
pub async fn fetch_to_file_cached_with_progress(
    client: &reqwest::Client,
    cache: &CacheRepository,
    candidates: &[String],
    dest: &Path,
    expected: &Expected,
    on_chunk: impl FnMut(u64),
) -> Result<(), FetchError> {
    if already_satisfied(dest, expected).await {
        return Ok(());
    }
    if let Some(sha1) = &expected.sha1 {
        if cache.link_from_cache(sha1, dest).await.unwrap_or(false) {
            return Ok(());
        }
    }
    fetch_to_file_with_progress(client, candidates, dest, expected, on_chunk).await?;
    if let Some(sha1) = &expected.sha1 {
        let _ = cache.put(sha1, dest).await;
    }
    Ok(())
}

pub struct FetchJob {
    pub candidates: Vec<String>,
    pub dest: PathBuf,
    pub expected: Expected,
}

/// 有界并发跑一批下载任务，单个任务失败不影响其它任务，结果按任务顺序对应返回。
/// 每个任务先查内容寻址缓存——库文件和 assets 会被很多实例
/// 共用, 装第二个用同一批 mod/版本的实例时大概率全部缓存命中, 一个字节都不用下。
pub async fn fetch_all_cached(
    client: &reqwest::Client,
    cache: &CacheRepository,
    jobs: Vec<FetchJob>,
    concurrency: usize,
) -> Vec<(PathBuf, Result<(), FetchError>)> {
    fetch_all_cached_with_progress(client, cache, jobs, concurrency, None).await
}

pub async fn fetch_all_cached_with_progress(
    client: &reqwest::Client,
    cache: &CacheRepository,
    jobs: Vec<FetchJob>,
    concurrency: usize,
    progress: Option<(InstallStage, &ProgressSink)>,
) -> Vec<(PathBuf, Result<(), FetchError>)> {
    if let Some((stage, tx)) = progress {
        let _ = tx.send(ProgressEvent::StageStarted {
            stage,
            total: jobs.len(),
        });
    }

    stream::iter(jobs)
        .map(|job| {
            let client = client.clone();
            let progress = progress.map(|(stage, tx)| (stage, tx.clone()));
            async move {
                let dest = job.dest.clone();
                let total_bytes = job.expected.size;
                let result = fetch_to_file_cached_with_progress(
                    &client,
                    cache,
                    &job.candidates,
                    &job.dest,
                    &job.expected,
                    |chunk_bytes| {
                        if let Some((_, tx)) = &progress {
                            let _ = tx.send(ProgressEvent::Bytes {
                                path: dest.clone(),
                                chunk_bytes,
                                total_bytes,
                            });
                        }
                    },
                )
                .await;
                if let Some((stage, tx)) = &progress {
                    let _ = tx.send(ProgressEvent::TaskDone { stage: *stage });
                }
                (job.dest, result)
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hmcl-rs-test")
            .join(name)
            .join(format!("{:x}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn downloads_and_verifies_checksum() {
        let server = MockServer::start().await;
        let body = b"hello minecraft".to_vec();
        Mock::given(method("GET"))
            .and(path("/client.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dest = tmp_dir("downloads_and_verifies_checksum").join("client.jar");
        let client = reqwest::Client::new();
        let expected = Expected::sha1_and_size(sha1_hex(&body), body.len() as u64);

        fetch_to_file(
            &client,
            &[format!("{}/client.jar", server.uri())],
            &dest,
            &expected,
        )
        .await
        .expect("download should succeed");

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
    }

    #[tokio::test]
    async fn hashes_files_without_loading_them_whole() {
        let path = tmp_dir("streaming_sha1").join("large.bin");
        let body = vec![0x5a; 192 * 1024 + 17];
        tokio::fs::write(&path, &body).await.unwrap();
        assert_eq!(sha1_file(&path).await.unwrap(), sha1_hex(&body));
    }

    #[tokio::test]
    async fn checksum_mismatch_is_rejected_and_no_partial_file_left_behind() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bad.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"wrong content".to_vec()))
            .mount(&server)
            .await;

        let dest = tmp_dir("checksum_mismatch").join("bad.jar");
        let client = reqwest::Client::new();
        let expected = Expected::sha1("0000000000000000000000000000000000000000");

        let err = fetch_to_file(
            &client,
            &[format!("{}/bad.jar", server.uri())],
            &dest,
            &expected,
        )
        .await
        .expect_err("checksum mismatch must fail");
        assert!(matches!(err, FetchError::AllCandidatesFailed { .. }));
        assert!(
            !dest.exists(),
            "destination file must not exist after a failed verify"
        );
        assert!(
            !part_file(&dest).exists(),
            ".part temp file must be cleaned up"
        );
    }

    #[tokio::test]
    async fn falls_back_to_second_candidate_when_first_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mirror-a/file.txt"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let body = b"from mirror b".to_vec();
        Mock::given(method("GET"))
            .and(path("/mirror-b/file.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dest = tmp_dir("candidate_fallback").join("file.txt");
        let client = reqwest::Client::new();
        let candidates = vec![
            format!("{}/mirror-a/file.txt", server.uri()),
            format!("{}/mirror-b/file.txt", server.uri()),
        ];

        fetch_to_file(
            &client,
            &candidates,
            &dest,
            &Expected::sha1(sha1_hex(&body)),
        )
        .await
        .expect("should fall through to the second, working candidate");
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
    }

    #[tokio::test]
    async fn skips_network_when_file_already_matches_expected_hash() {
        let dir = tmp_dir("already_satisfied");
        let dest = dir.join("already-there.bin");
        let body = b"already correct on disk".to_vec();
        tokio::fs::write(&dest, &body).await.unwrap();

        let server = MockServer::start().await;
        let client = reqwest::Client::new();
        let expected = Expected::sha1_and_size(sha1_hex(&body), body.len() as u64);

        fetch_to_file(
            &client,
            &[format!("{}/should-not-be-called", server.uri())],
            &dest,
            &expected,
        )
        .await
        .expect("pre-existing correct file must short-circuit before hitting the network");
    }

    #[tokio::test]
    async fn progress_events_report_stage_total_bytes_and_task_done_for_real_downloads() {
        let server = MockServer::start().await;
        let body_a = b"first file bytes".to_vec();
        let body_b = b"second file, a bit longer than the first one".to_vec();
        Mock::given(method("GET"))
            .and(path("/a.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body_a.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body_b.clone()))
            .mount(&server)
            .await;

        let dir = tmp_dir("progress_real_downloads");
        let dest_a = dir.join("a.bin");
        let dest_b = dir.join("b.bin");
        let client = reqwest::Client::new();
        let cache = super::super::CacheRepository::new(dir.join("cache"));
        let jobs = vec![
            FetchJob {
                candidates: vec![format!("{}/a.bin", server.uri())],
                dest: dest_a.clone(),
                expected: Expected::sha1_and_size(sha1_hex(&body_a), body_a.len() as u64),
            },
            FetchJob {
                candidates: vec![format!("{}/b.bin", server.uri())],
                dest: dest_b.clone(),
                expected: Expected::sha1_and_size(sha1_hex(&body_b), body_b.len() as u64),
            },
        ];

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let results = fetch_all_cached_with_progress(
            &client,
            &cache,
            jobs,
            4,
            Some((InstallStage::Libraries, &tx)),
        )
        .await;
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        drop(tx);

        let mut saw_stage_started = false;
        let mut task_done_count = 0;
        let mut bytes_by_path: std::collections::HashMap<PathBuf, u64> =
            std::collections::HashMap::new();
        while let Some(event) = rx.recv().await {
            match event {
                ProgressEvent::StageStarted { stage, total } => {
                    assert_eq!(stage, InstallStage::Libraries);
                    assert_eq!(
                        total, 2,
                        "StageStarted.total must be the job count, known up front"
                    );
                    saw_stage_started = true;
                }
                ProgressEvent::TaskDone { stage } => {
                    assert_eq!(stage, InstallStage::Libraries);
                    task_done_count += 1;
                }
                ProgressEvent::Bytes {
                    path,
                    chunk_bytes,
                    total_bytes,
                } => {
                    let expected = if path == dest_a {
                        body_a.len()
                    } else {
                        body_b.len()
                    };
                    assert_eq!(total_bytes, Some(expected as u64));
                    *bytes_by_path.entry(path).or_default() += chunk_bytes;
                }
                ProgressEvent::LoaderStarted { .. } | ProgressEvent::LoaderFinished => {}
            }
        }

        assert!(saw_stage_started);
        assert_eq!(
            task_done_count, 2,
            "one TaskDone per job, regardless of order"
        );
        assert_eq!(
            bytes_by_path.get(&dest_a).copied(),
            Some(body_a.len() as u64),
            "Bytes events for a real download must sum to the actual file size"
        );
        assert_eq!(
            bytes_by_path.get(&dest_b).copied(),
            Some(body_b.len() as u64)
        );
    }

    #[tokio::test]
    async fn progress_skips_bytes_events_for_a_file_already_satisfied_on_disk() {
        let dir = tmp_dir("progress_already_satisfied");
        let dest = dir.join("already-there.bin");
        let body = b"nothing to download here".to_vec();
        tokio::fs::write(&dest, &body).await.unwrap();

        let server = MockServer::start().await;
        let client = reqwest::Client::new();
        let cache = super::super::CacheRepository::new(dir.join("cache"));
        let jobs = vec![FetchJob {
            candidates: vec![format!("{}/should-not-be-called", server.uri())],
            dest: dest.clone(),
            expected: Expected::sha1_and_size(sha1_hex(&body), body.len() as u64),
        }];

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let results = fetch_all_cached_with_progress(
            &client,
            &cache,
            jobs,
            1,
            Some((InstallStage::AssetObjects, &tx)),
        )
        .await;
        assert!(results[0].1.is_ok());
        drop(tx);

        let mut saw_task_done = false;
        while let Some(event) = rx.recv().await {
            assert!(
                !matches!(event, ProgressEvent::Bytes { .. }),
                "a file already on disk never streams bytes, so it must never emit Bytes"
            );
            if matches!(event, ProgressEvent::TaskDone { .. }) {
                saw_task_done = true;
            }
        }
        assert!(
            saw_task_done,
            "a cache-satisfied task must still count toward the N/M completion total"
        );
    }
}

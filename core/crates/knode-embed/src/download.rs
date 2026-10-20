use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::model::{MANIFEST_FILE, ModelFile, ModelSpec, models_dir};
use crate::{Error, Result};

/// The slowest average speed a model file may arrive at: 256 KiB/s.
const MIN_BYTES_PER_SECOND: u64 = 256 * 1024;

/// How long a file of `size` bytes may take to arrive once its download starts: at least a
/// minute, and long enough at [`MIN_BYTES_PER_SECOND`]. ureq's body timeout covers the whole
/// body, not each read, so one fixed value would fail every large file on a slow connection.
fn body_timeout(size: u64) -> Duration {
    Duration::from_secs(60.max(size.div_ceil(MIN_BYTES_PER_SECOND)))
}

/// Where a model download is, reported to the callback of [`ensure_model`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// A file started downloading.
    Started {
        /// The file name.
        file: String,
        /// Its size in bytes.
        total: u64,
    },
    /// More of a file arrived.
    Downloading {
        /// The file name.
        file: String,
        /// Bytes received so far for this file.
        done: u64,
        /// Its size in bytes.
        total: u64,
    },
    /// A file arrived and its SHA-256 matches.
    Verified {
        /// The file name.
        file: String,
    },
    /// A file was already in place and verified, so it was not downloaded again.
    Skipped {
        /// The file name.
        file: String,
    },
    /// Every file is in place: the model is ready to load.
    Ready {
        /// The model folder.
        dir: PathBuf,
    },
}

/// Makes sure the model is in `$KNODE_HOME/models/<id>`, downloading what is missing, and returns
/// its folder. A model already complete is returned at once, without network access.
pub fn ensure_model(spec: &ModelSpec, progress: impl FnMut(&Progress)) -> Result<PathBuf> {
    Downloader::new().ensure(spec, &models_dir()?, progress)
}

/// Downloads model files from Hugging Face, or from a mirror with the same layout
/// (`<base>/<repo>/resolve/<revision>/<file>`).
#[derive(Debug, Clone)]
pub struct Downloader {
    base_url: String,
    agent: ureq::Agent,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader {
    /// A downloader for `https://huggingface.co`.
    pub fn new() -> Downloader {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Downloader {
            base_url: "https://huggingface.co".into(),
            agent,
        }
    }

    /// The same downloader, reading from another server with the same layout.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Downloader {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Makes sure the model is complete in `models_dir/<id>` and returns that folder.
    ///
    /// Each file is written to a temporary name, checked against its SHA-256 and only then moved
    /// into place; a file that fails the check is deleted and the call fails with
    /// [`Error::Checksum`]. The manifest is written last, so an interrupted download is resumed
    /// file by file on the next call.
    pub fn ensure(
        &self,
        spec: &ModelSpec,
        models_dir: &Path,
        mut progress: impl FnMut(&Progress),
    ) -> Result<PathBuf> {
        let dir = models_dir.join(&spec.id);
        if dir.join(MANIFEST_FILE).is_file() {
            progress(&Progress::Ready { dir: dir.clone() });
            return Ok(dir);
        }
        std::fs::create_dir_all(&dir).map_err(|err| Error::io(&dir, err))?;
        for file in &spec.files {
            let path = dir.join(&file.name);
            if path.is_file() && sha256_of(&path)? == file.sha256 {
                progress(&Progress::Skipped {
                    file: file.name.clone(),
                });
                continue;
            }
            self.download(spec, file, &path, &mut progress)?;
        }
        spec.manifest().write(&dir)?;
        progress(&Progress::Ready { dir: dir.clone() });
        Ok(dir)
    }

    fn download(
        &self,
        spec: &ModelSpec,
        file: &ModelFile,
        path: &Path,
        progress: &mut impl FnMut(&Progress),
    ) -> Result<()> {
        let url = format!(
            "{}/{}/resolve/{}/{}",
            self.base_url, spec.repo, spec.revision, file.name
        );
        let failed = |message: String| Error::Download {
            file: file.name.clone(),
            message,
        };
        progress(&Progress::Started {
            file: file.name.clone(),
            total: file.size,
        });
        let mut response = self
            .agent
            .get(&url)
            .config()
            .timeout_recv_body(Some(body_timeout(file.size)))
            .build()
            .call()
            .map_err(|err| failed(err.to_string()))?;
        // One byte over the expected size is enough to tell a wrong file apart.
        let mut body = response
            .body_mut()
            .with_config()
            .limit(file.size + 1)
            .reader();

        let tmp = path.with_file_name(format!("{}.{}.part", file.name, std::process::id()));
        let result = (|| {
            let mut out = File::create(&tmp).map_err(|err| Error::io(&tmp, err))?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 1 << 16];
            let mut done = 0u64;
            let mut last_report = 0u64;
            loop {
                let n = match body.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(err) => return Err(failed(err.to_string())),
                };
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])
                    .map_err(|err| Error::io(&tmp, err))?;
                done += n as u64;
                if done - last_report >= 1 << 20 || done == file.size {
                    last_report = done;
                    progress(&Progress::Downloading {
                        file: file.name.clone(),
                        done,
                        total: file.size,
                    });
                }
            }
            out.sync_all().map_err(|err| Error::io(&tmp, err))?;
            let actual = hex(&hasher.finalize());
            if actual != file.sha256 {
                return Err(Error::Checksum {
                    file: file.name.clone(),
                    expected: file.sha256.clone(),
                    actual,
                });
            }
            std::fs::rename(&tmp, path).map_err(|err| Error::io(path, err))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else {
            progress(&Progress::Verified {
                file: file.name.clone(),
            });
        }
        result
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|err| Error::io(path, err))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|err| Error::io(path, err))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model::Pooling;

    /// A tiny HTTP server that answers `GET <path>` with fixed bytes and counts requests.
    fn serve(files: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<usize>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                *counter.lock().unwrap() += 1;
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                match files.get(&path) {
                    Some(body) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(head.as_bytes()).unwrap();
                        stream.write_all(body).unwrap();
                    }
                    None => {
                        stream
                            .write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .unwrap();
                    }
                }
            }
        });
        (base, hits)
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn spec(files: &[(&str, &[u8])]) -> ModelSpec {
        ModelSpec {
            id: "fake".into(),
            repo: "org/fake".into(),
            revision: "abc".into(),
            files: files
                .iter()
                .map(|(name, bytes)| ModelFile {
                    name: name.to_string(),
                    size: bytes.len() as u64,
                    sha256: sha(bytes),
                })
                .collect(),
            dimension: 4,
            max_tokens: 16,
            query_prefix: String::new(),
            passage_prefix: String::new(),
            pooling: Pooling::Mean,
        }
    }

    #[test]
    fn a_large_file_has_time_to_arrive_on_a_slow_connection() {
        assert_eq!(body_timeout(0), Duration::from_secs(60));
        assert_eq!(body_timeout(1 << 20), Duration::from_secs(60));
        // The model's weights, about 470 MB, get half an hour.
        let weights = body_timeout(470_641_600);
        assert!(weights >= Duration::from_secs(29 * 60), "{weights:?}");
        assert_eq!(
            weights.as_secs(),
            470_641_600_u64.div_ceil(MIN_BYTES_PER_SECOND)
        );
    }

    #[test]
    fn downloads_verifies_and_writes_the_manifest() {
        let a = b"first file".to_vec();
        let b = vec![7u8; 3 << 20];
        let (base, hits) = serve(HashMap::from([
            ("/org/fake/resolve/abc/a.json".to_string(), a.clone()),
            ("/org/fake/resolve/abc/b.bin".to_string(), b.clone()),
        ]));
        let spec = spec(&[("a.json", &a), ("b.bin", &b)]);
        let home = tempfile::tempdir().unwrap();
        let mut events = Vec::new();
        let dir = Downloader::new()
            .with_base_url(&base)
            .ensure(&spec, home.path(), |p| events.push(p.clone()))
            .unwrap();

        assert_eq!(dir, home.path().join("fake"));
        assert_eq!(std::fs::read(dir.join("a.json")).unwrap(), a);
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b);
        let manifest = crate::Manifest::read(&dir).unwrap();
        assert_eq!(manifest.id, "fake");
        assert_eq!(manifest.source, "org/fake@abc");
        assert!(events.contains(&Progress::Verified {
            file: "b.bin".into()
        }));
        assert!(events.iter().any(|p| matches!(
            p,
            Progress::Downloading { file, done, total } if file == "b.bin" && done == total
        )));
        assert_eq!(events.last(), Some(&Progress::Ready { dir: dir.clone() }));
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .count();
        assert_eq!(leftovers, 0);

        // A complete model is not downloaded again.
        let before = *hits.lock().unwrap();
        Downloader::new()
            .with_base_url(&base)
            .ensure(&spec, home.path(), |_| {})
            .unwrap();
        assert_eq!(*hits.lock().unwrap(), before);
    }

    #[test]
    fn a_wrong_file_is_rejected_and_deleted() {
        let (base, _) = serve(HashMap::from([(
            "/org/fake/resolve/abc/a.json".to_string(),
            b"tampered!!".to_vec(),
        )]));
        let spec = spec(&[("a.json", b"first file")]);
        let home = tempfile::tempdir().unwrap();
        let err = Downloader::new()
            .with_base_url(&base)
            .ensure(&spec, home.path(), |_| {})
            .unwrap_err();
        assert!(
            matches!(err, Error::Checksum { ref file, .. } if file == "a.json"),
            "{err}"
        );
        let dir = home.path().join("fake");
        assert!(!dir.join("a.json").exists());
        assert!(!dir.join(MANIFEST_FILE).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn a_missing_file_is_a_clear_error() {
        let (base, _) = serve(HashMap::new());
        let spec = spec(&[("a.json", b"first file")]);
        let home = tempfile::tempdir().unwrap();
        let err = Downloader::new()
            .with_base_url(&base)
            .ensure(&spec, home.path(), |_| {})
            .unwrap_err();
        assert!(
            matches!(err, Error::Download { ref file, .. } if file == "a.json"),
            "{err}"
        );
        assert!(err.to_string().contains("404"), "{err}");
    }

    #[test]
    fn verified_files_already_there_are_kept() {
        let (base, hits) = serve(HashMap::new());
        let spec = spec(&[("a.json", b"first file")]);
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("fake");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), b"first file").unwrap();
        let mut events = Vec::new();
        Downloader::new()
            .with_base_url(&base)
            .ensure(&spec, home.path(), |p| events.push(p.clone()))
            .unwrap();
        assert_eq!(*hits.lock().unwrap(), 0);
        assert_eq!(
            events[0],
            Progress::Skipped {
                file: "a.json".into()
            }
        );
    }

    #[test]
    fn the_default_model_is_pinned() {
        let spec = ModelSpec::default_model();
        assert_eq!(spec.revision.len(), 40);
        assert!(spec.files.iter().all(|f| f.sha256.len() == 64));
        assert!(spec.download_size() > 400_000_000);
    }
}

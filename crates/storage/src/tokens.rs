//! 账户令牌持久化。
//!
//! 文件权限固定为 0600；写入使用临时文件 + fsync + rename。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::keyring;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 一组账户令牌。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    /// 最近一次 Minecraft 访问令牌。
    #[serde(default)]
    pub access_token: Option<String>,
    /// OAuth 刷新令牌。
    pub refresh_token: String,
    /// 访问令牌过期时间（Unix 秒）。
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    tokens: BTreeMap<String, TokenSet>,
}

/// 令牌存储错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// JSON 解析错误。
    Json {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: serde_json::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
        }
    }
}

/// 令牌文件存储。
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// 创建令牌存储。
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 列出全部令牌。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn list(&self) -> Result<Vec<(String, TokenSet)>, Error> {
        Ok(self.load()?.tokens.into_iter().collect())
    }

    /// 读取指定账户的令牌。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn get(&self, id: &str) -> Result<Option<TokenSet>, Error> {
        if keyring::enabled() {
            if let Some(secret) = keyring::lookup(id) {
                if let Ok(token) = serde_json::from_str::<TokenSet>(&secret) {
                    return Ok(Some(token));
                }
            }
        }
        Ok(self.load()?.tokens.get(id).cloned())
    }

    /// 写入指定账户的令牌。
    ///
    /// # Errors
    ///
    /// 读取或写入失败时返回错误。
    pub fn set(&self, id: &str, token: &TokenSet) -> Result<(), Error> {
        if keyring::enabled() {
            if let Ok(secret) = serde_json::to_string(token) {
                if keyring::store(id, &secret) {
                    let mut file = self.load()?;
                    if file.tokens.remove(id).is_some() {
                        self.save(&file)?;
                    }
                    return Ok(());
                }
            }
        }
        let mut file = self.load()?;
        file.tokens.insert(id.to_owned(), token.clone());
        self.save(&file)
    }

    /// 删除指定账户的令牌。
    ///
    /// # Errors
    ///
    /// 读取或写入失败时返回错误。
    pub fn remove(&self, id: &str) -> Result<(), Error> {
        if keyring::enabled() {
            let _ = keyring::clear(id);
        }
        let mut file = self.load()?;
        if file.tokens.remove(id).is_some() {
            self.save(&file)?;
        }
        Ok(())
    }

    fn load(&self) -> Result<File, Error> {
        if !self.path.exists() {
            return Ok(File::default());
        }
        let bytes = fs::read(&self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })
    }

    fn save(&self, file: &File) -> Result<(), Error> {
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".tokens-{}-{counter}.tmp", std::process::id()));
        let result = (|| {
            let bytes = serde_json::to_vec_pretty(file).map_err(|source| Error::Json {
                path: self.path.clone(),
                source,
            })?;
            let mut handle = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|source| Error::Io {
                    path: temp.clone(),
                    source,
                })?;
            handle.write_all(&bytes).map_err(|source| Error::Io {
                path: temp.clone(),
                source,
            })?;
            handle.sync_all().map_err(|source| Error::Io {
                path: temp.clone(),
                source,
            })?;
            drop(handle);
            fs::rename(&temp, &self.path).map_err(|source| Error::Io {
                path: self.path.clone(),
                source,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Store, TokenSet};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-tokens-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn round_trips_tokens_and_sets_permissions() {
        let directory = temp_dir("store");
        let path = directory.join("tokens.json");
        let store = Store::new(path.clone());
        store
            .set(
                "account",
                &TokenSet {
                    refresh_token: "refresh".to_owned(),
                    access_token: Some("access".to_owned()),
                    expires_at: Some(42),
                },
            )
            .expect("set");
        assert_eq!(
            store
                .get("account")
                .expect("get")
                .expect("token")
                .refresh_token,
            "refresh"
        );
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        store.remove("account").expect("remove");
        assert!(store.get("account").expect("get").is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn writes_fields_in_config_order() {
        let directory = temp_dir("order");
        let store = Store::new(directory.join("tokens.json"));
        store
            .set(
                "account",
                &TokenSet {
                    access_token: Some("access".to_owned()),
                    refresh_token: "refresh".to_owned(),
                    expires_at: Some(1),
                },
            )
            .expect("set");
        let text = fs::read_to_string(directory.join("tokens.json")).expect("read");
        let mut last = 0;
        for key in ["access_token", "refresh_token", "expires_at"] {
            let position = text.find(&format!("\"{key}\"")).expect(key);
            assert!(position >= last, "field order: {key}");
            last = position;
        }
        let _ = fs::remove_dir_all(&directory);
    }
}

//! 账户索引与离线 UUID。
//!
//! `accounts.json` 只保存元数据，不保存令牌。
//! 令牌由运行时内存或后续密钥环实现持有。

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fs::FileSystem;

/// 账户标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    /// 校验并构造账户标识。
    ///
    /// # Errors
    ///
    /// 为空时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::InvalidId);
        }
        Ok(Self(value))
    }

    /// 字符串形式。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 账户类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// 离线账户。
    Offline,
    /// 外置认证（authlib-injector 等服务）。
    External,
    /// 微软账户。
    Microsoft,
}

/// 账户元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// 账户标识（游戏 UUID）。
    pub id: AccountId,
    /// 账户类型。
    pub kind: Kind,
    /// 显示名称。
    pub name: String,
    /// 外置认证服务器。
    #[serde(default)]
    pub server: Option<String>,
    /// 创建时间（Unix 秒）。
    #[serde(default)]
    pub created_at: u64,
    /// 最近使用时间（Unix 秒）。
    #[serde(default)]
    pub last_used_at: Option<u64>,
    /// 累计运行秒数。
    #[serde(default)]
    pub run_seconds: u64,
}

/// `accounts.json` 中的账户记录。
///
/// 文件保存 `id`。
#[derive(Debug, Serialize, Deserialize)]
struct AccountRecord {
    id: String,
    kind: Kind,
    name: String,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    last_used_at: Option<u64>,
    #[serde(default)]
    run_seconds: u64,
}

impl AccountRecord {
    fn from_account(account: &Account) -> Self {
        Self {
            id: account.id.as_str().to_owned(),
            kind: account.kind,
            name: account.name.clone(),
            server: account.server.clone(),
            created_at: account.created_at,
            last_used_at: account.last_used_at,
            run_seconds: account.run_seconds,
        }
    }

    fn into_account(self) -> Result<Account, Error> {
        let id = AccountId::new(self.id).map_err(|_| Error::InvalidId)?;
        Ok(Account {
            id,
            kind: self.kind,
            name: self.name,
            server: self.server,
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            run_seconds: self.run_seconds,
        })
    }
}

/// 账户错误。
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
    /// 账户标识非法。
    InvalidId,
    /// 名称非法。
    InvalidName,
    /// 认证服务器地址非法。
    InvalidServer,
    /// 账户已存在。
    Duplicate(String),
    /// 账户不存在。
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidId => formatter.write_str("account id is empty"),
            Self::InvalidName => formatter.write_str("account name is empty or too long"),
            Self::InvalidServer => formatter.write_str("server must be an http(s) URL"),
            Self::Duplicate(id) => write!(formatter, "account already exists: {id}"),
            Self::NotFound(id) => write!(formatter, "account not found: {id}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::InvalidId
            | Self::InvalidName
            | Self::InvalidServer
            | Self::Duplicate(_)
            | Self::NotFound(_) => None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    accounts: Vec<AccountRecord>,
}

/// 账户存储。
pub struct Store<F: FileSystem> {
    path: PathBuf,
    fs: F,
}

impl<F: FileSystem> Store<F> {
    /// 创建账户存储。
    #[must_use]
    pub fn new(path: PathBuf, fs: F) -> Self {
        Self { path, fs }
    }

    /// 读取全部账户。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn list(&self) -> Result<Vec<Account>, Error> {
        if !self.fs.exists(&self.path) {
            return Ok(Vec::new());
        }
        let bytes = self.fs.read(&self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;
        let file: File = serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })?;
        file.accounts
            .into_iter()
            .map(AccountRecord::into_account)
            .collect()
    }

    /// 按标识读取账户。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn get(&self, id: &AccountId) -> Result<Option<Account>, Error> {
        Ok(self.list()?.into_iter().find(|account| account.id == *id))
    }

    /// 添加账户。
    ///
    /// # Errors
    ///
    /// 标识重复、名称非法或写入失败时返回错误。
    pub fn add(&self, account: &Account) -> Result<(), Error> {
        validate_name(&account.name)?;
        if let Some(server) = &account.server {
            validate_server(server)?;
        }
        let mut accounts = self.list()?;
        if accounts.iter().any(|existing| existing.id == account.id) {
            return Err(Error::Duplicate(account.id.to_string()));
        }
        accounts.push(account.clone());
        self.save(&accounts)
    }

    /// 更新已有账户的元数据。
    ///
    /// # Errors
    ///
    /// 账户不存在、名称/服务器非法或写入失败时返回错误。
    pub fn update(&self, account: &Account) -> Result<(), Error> {
        validate_name(&account.name)?;
        if let Some(server) = &account.server {
            validate_server(server)?;
        }
        let mut accounts = self.list()?;
        let Some(existing) = accounts
            .iter_mut()
            .find(|existing| existing.id == account.id)
        else {
            return Err(Error::NotFound(account.id.to_string()));
        };
        *existing = account.clone();
        self.save(&accounts)
    }

    /// 删除账户。
    ///
    /// # Errors
    ///
    /// 账户不存在或写入失败时返回错误。
    pub fn remove(&self, id: &AccountId) -> Result<(), Error> {
        let mut accounts = self.list()?;
        let before = accounts.len();
        accounts.retain(|account| account.id != *id);
        if accounts.len() == before {
            return Err(Error::NotFound(id.to_string()));
        }
        self.save(&accounts)
    }

    fn save(&self, accounts: &[Account]) -> Result<(), Error> {
        let file = File {
            accounts: accounts.iter().map(AccountRecord::from_account).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })?;
        self.fs
            .write(&self.path, &bytes)
            .map_err(|source| Error::Io {
                path: self.path.clone(),
                source,
            })
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(Error::InvalidName);
    }
    Ok(())
}

fn validate_server(server: &str) -> Result<(), Error> {
    if server.starts_with("https://") || server.starts_with("http://") {
        Ok(())
    } else {
        Err(Error::InvalidServer)
    }
}

/// 根据离线名称生成 UUID（`OfflinePlayer:<name>` 的 MD5）。
#[must_use]
pub fn offline_uuid(name: &str) -> String {
    let digest = md5::compute(format!("OfflinePlayer:{name}"));
    let mut bytes = digest.0;
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// 外置账户服务器地址。
#[must_use]
pub fn normalise_server(server: &str) -> String {
    server.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Account, AccountId, Kind, Store, offline_uuid};
    use crate::fs::LocalFileSystem;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-accounts-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn offline_uuid_is_stable_and_versioned() {
        let first = offline_uuid("Player");
        let second = offline_uuid("Player");
        assert_eq!(first, second);
        assert_eq!(&first[14..15], "3");
        assert!(first.contains('-'));
        assert_ne!(first, offline_uuid("Other"));
    }

    #[test]
    fn stores_and_removes_accounts() {
        let directory = temp_dir("store");
        let store = Store::new(directory.join("accounts.json"), LocalFileSystem);
        let account = Account {
            id: AccountId::new(offline_uuid("Player")).expect("id"),
            kind: Kind::Offline,
            name: "Player".to_owned(),
            server: None,
            created_at: 0,
            last_used_at: None,
            run_seconds: 0,
        };
        store.add(&account).expect("add");
        assert_eq!(store.list().expect("list").len(), 1);
        assert!(store.add(&account).is_err());
        store.remove(&account.id).expect("remove");
        assert_eq!(store.list().expect("list").len(), 0);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn writes_id_without_uuid() {
        let directory = temp_dir("id-only");
        let store = Store::new(directory.join("accounts.json"), LocalFileSystem);
        let uuid = offline_uuid("Player");
        let account = Account {
            id: AccountId::new(uuid.clone()).expect("id"),
            kind: Kind::Offline,
            name: "Player".to_owned(),
            server: None,
            created_at: 0,
            last_used_at: None,
            run_seconds: 0,
        };
        store.add(&account).expect("add");
        let text = fs::read_to_string(directory.join("accounts.json")).expect("read");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let entry = &value["accounts"][0];
        assert!(entry.get("uuid").is_none());
        assert_eq!(entry["id"], uuid);
        let keys = [
            "id",
            "kind",
            "name",
            "server",
            "created_at",
            "last_used_at",
            "run_seconds",
        ];
        let mut last = 0;
        for key in keys {
            let position = text.find(&format!("\"{key}\"")).expect(key);
            assert!(position >= last, "field order: {key}");
            last = position;
        }
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_invalid_server() {
        let directory = temp_dir("server");
        let store = Store::new(directory.join("accounts.json"), LocalFileSystem);
        let account = Account {
            id: AccountId::new("id").expect("id"),
            kind: Kind::External,
            name: "Player".to_owned(),
            server: Some("ftp://example.com".to_owned()),
            created_at: 0,
            last_used_at: None,
            run_seconds: 0,
        };
        assert!(store.add(&account).is_err());
        let _ = fs::remove_dir_all(&directory);
    }
}

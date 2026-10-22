//! 账号仓库：`%LOCALAPPDATA%\CodexSwitch\index.json` + `profiles\<账号ID>\auth.json`。

use crate::identity::{self, Identity};
use crate::win;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    #[serde(default)]
    pub email: Option<String>,
    pub added_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

impl Account {
    pub fn label(&self) -> String {
        match &self.email {
            Some(email) if !email.trim().is_empty() => email.clone(),
            _ => identity::mask(&self.id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            version: 1,
            active: None,
            accounts: Vec::new(),
        }
    }
}

pub struct Store {
    pub root: PathBuf,
    pub codex_home: PathBuf,
}

impl Store {
    pub fn discover() -> Result<Self> {
        let local = std::env::var_os("LOCALAPPDATA").context("找不到 LOCALAPPDATA 环境变量")?;
        let root = std::env::var_os("CODEX_SWITCH_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(local).join("CodexSwitch"));
        let codex_home = match std::env::var_os("CODEX_HOME") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                let profile =
                    std::env::var_os("USERPROFILE").context("找不到 USERPROFILE 环境变量")?;
                PathBuf::from(profile).join(".codex")
            }
        };
        Ok(Self { root, codex_home })
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn active_auth(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    pub fn profile_auth(&self, account_id: &str) -> PathBuf {
        self.profiles_dir()
            .join(directory_name(account_id))
            .join("auth.json")
    }

    pub fn load(&self) -> Result<Index> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(Index::default());
        }
        let bytes = std::fs::read(&path).context("读取 index.json 失败")?;
        let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &bytes[3..]
        } else {
            &bytes[..]
        };
        serde_json::from_slice(bytes).context("index.json 内容损坏")
    }

    pub fn save(&self, index: &Index) -> Result<()> {
        std::fs::create_dir_all(self.profiles_dir())?;
        let bytes = serde_json::to_vec_pretty(index)?;
        win::write_atomic(&self.index_path(), &bytes)
    }

    pub fn read_active_identity(&self) -> Result<Identity> {
        let path = self.active_auth();
        if !path.exists() {
            anyhow::bail!("活动目录里没有 auth.json：{}", path.display());
        }
        let bytes = std::fs::read(&path).context("读取活动 auth.json 失败")?;
        identity::parse_auth_json(&bytes)
    }

    /// 把「当前正在登录的账号」存成一个 profile，并标记为当前账号。
    pub fn register_current(&self) -> Result<Account> {
        let path = self.active_auth();
        let bytes =
            std::fs::read(&path).with_context(|| format!("读取失败：{}", path.display()))?;
        let identity = identity::parse_auth_json(&bytes)?;
        let mut index = self.load()?;
        let account = self.upsert(&mut index, &bytes, &identity)?;
        index.active = Some(account.id.clone());
        self.save(&index)?;
        Ok(account)
    }

    /// 从任意 auth.json 导入（配合 `CODEX_HOME=<临时目录> codex login` 使用）。
    pub fn import(&self, source: &Path) -> Result<Account> {
        let bytes =
            std::fs::read(source).with_context(|| format!("读取失败：{}", source.display()))?;
        let identity =
            identity::parse_auth_json(&bytes).context("这个文件不是有效的 Codex auth.json")?;
        let mut index = self.load()?;
        let account = self.upsert(&mut index, &bytes, &identity)?;
        self.save(&index)?;
        Ok(account)
    }

    /// 删除一个非当前账号，连同它的凭证快照。
    pub fn remove_account(&self, account_id: &str) -> Result<()> {
        let mut index = self.load()?;
        if index.active.as_deref() == Some(account_id) {
            anyhow::bail!("当前账号不能删除，请先切换到其它账号");
        }
        let Some(position) = index
            .accounts
            .iter()
            .position(|account| account.id == account_id)
        else {
            anyhow::bail!("找不到该账号");
        };
        index.accounts.remove(position);
        self.save(&index)?;
        let directory = self.profiles_dir().join(directory_name(account_id));
        if directory.exists() {
            std::fs::remove_dir_all(&directory)
                .with_context(|| format!("删除凭证目录失败：{}", directory.display()))?;
        }
        Ok(())
    }

    fn upsert(&self, index: &mut Index, bytes: &[u8], identity: &Identity) -> Result<Account> {
        let profile = self.profile_auth(&identity.account_id);
        win::write_atomic(&profile, bytes)?;

        if let Some(existing) = index
            .accounts
            .iter_mut()
            .find(|account| account.id == identity.account_id)
        {
            existing.email = identity.email.clone();
            return Ok(existing.clone());
        }

        let account = Account {
            id: identity.account_id.clone(),
            email: identity.email.clone(),
            added_at: now_seconds(),
            last_used_at: None,
        };
        index.accounts.push(account.clone());
        Ok(account)
    }
}

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn directory_name(account_id: &str) -> String {
    account_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

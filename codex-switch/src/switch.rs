//! 默认通过 stdio 桥接器在当前轮结束后更新后台认证，并验证后提交磁盘状态。
//! 连接缺失时先自动重新接通原账号，再通过后台执行无感切号。

use crate::identity;
use crate::store::{now_seconds, Account, Index, Store};
use crate::win;
use anyhow::{bail, Context, Result};

pub struct Outcome {
    pub queued: bool,
}

pub fn switch_to(store: &Store, query: &str, dry_run: bool) -> Result<Outcome> {
    switch_with_progress(store, query, dry_run, |message| println!("{message}"))
}

pub fn switch_with_progress(
    store: &Store,
    query: &str,
    dry_run: bool,
    progress: impl Fn(&str),
) -> Result<Outcome> {
    let prepared = Prepared::load(store, query)?;
    if dry_run {
        println!(
            "演练：切换到 {}；不会关闭窗口或修改账号。",
            prepared.target.label()
        );
        let sessions = crate::bridge::sessions(store)?;
        println!("可用无感后台：{}", sessions.len());
        return Ok(Outcome { queued: false });
    }
    crate::connection::ensure(store, progress)?;
    let sessions = crate::bridge::sessions(store)?;
    if sessions.len() > 1 {
        bail!("发现多个使用同一 Codex 目录的后台，暂不支持同时切换；请只保留一个 Desktop 后台。");
    }
    if let Some(session) = sessions.first() {
        let result = session.request("switch", Some(&prepared.target.id))?;
        return Ok(Outcome {
            queued: result.get("state").and_then(serde_json::Value::as_str) == Some("queued"),
        });
    }
    bail!("无感后台未连接，账号未改动")
}

/// Prepared again when an idle boundary is reached, so queued requests never commit stale data.
pub(crate) struct Prepared {
    pub target: Account,
    pub target_bytes: Vec<u8>,
    pub previous_bytes: Vec<u8>,
    original_index: Index,
}

impl Prepared {
    pub fn load(store: &Store, query: &str) -> Result<Self> {
        let index = store.load()?;
        let target = resolve(&index, query)?.clone();
        let previous_bytes =
            std::fs::read(store.active_auth()).context("无法读取当前凭据；请先登记当前登录")?;
        let previous = identity::parse_auth_json(&previous_bytes)?;
        if index.active.as_deref() != Some(previous.account_id.as_str()) {
            bail!("当前凭据与登记状态不一致；请先登记当前登录，账号未改动");
        }
        if previous.account_id == target.id {
            bail!("该账号已经是当前账号");
        }
        let target_bytes =
            std::fs::read(store.profile_auth(&target.id)).context("无法读取目标账号凭据")?;
        if !identity::parse_auth_json(&target_bytes)?.matches(&target.id, target.email.as_deref()) {
            bail!("目标凭据与登记账号不一致，拒绝切换");
        }
        Ok(Self {
            target,
            target_bytes,
            previous_bytes,
            original_index: index,
        })
    }

    pub fn commit(&self, store: &Store) -> Result<()> {
        let result = (|| -> Result<()> {
            let previous = identity::parse_auth_json(&self.previous_bytes)?;
            // Do not overwrite an independently changed login while a backend request was in flight.
            let current = std::fs::read(store.active_auth())?;
            if current != self.previous_bytes {
                bail!("切换期间当前凭据发生变化，已停止切换");
            }
            if serde_json::to_value(store.load()?)? != serde_json::to_value(&self.original_index)? {
                bail!("切换期间账号列表发生变化，已停止切换");
            }
            win::write_atomic(&store.profile_auth(&previous.account_id), &current)?;
            win::write_atomic(&store.active_auth(), &self.target_bytes)?;
            let after = identity::parse_auth_json(&std::fs::read(store.active_auth())?)?;
            if !after.matches(&self.target.id, self.target.email.as_deref()) {
                bail!("写入后的账号校验失败");
            }
            let mut index = self.original_index.clone();
            index.active = Some(self.target.id.clone());
            if let Some(account) = index.accounts.iter_mut().find(|a| a.id == self.target.id) {
                account.last_used_at = Some(now_seconds());
            }
            store.save(&index)
        })();
        if let Err(error) = result {
            // Only roll back our own write; never replace an unrelated concurrent login.
            if std::fs::read(store.active_auth()).ok().as_deref()
                == Some(self.target_bytes.as_slice())
            {
                win::write_atomic(&store.active_auth(), &self.previous_bytes)
                    .context("提交失败，恢复当前凭据也失败")?;
            }
            return Err(error);
        }
        Ok(())
    }
}

fn resolve<'a>(index: &'a Index, query: &str) -> Result<&'a Account> {
    if let Some(account) = index
        .accounts
        .iter()
        .find(|account| account.id.eq_ignore_ascii_case(query))
    {
        return Ok(account);
    }

    let needle = query.to_ascii_lowercase();
    if let Some(account) = index.accounts.iter().find(|account| {
        account
            .email
            .as_deref()
            .map(|email| email.eq_ignore_ascii_case(query))
            .unwrap_or(false)
    }) {
        return Ok(account);
    }

    let matches: Vec<&Account> = index
        .accounts
        .iter()
        .filter(|account| {
            account
                .email
                .as_deref()
                .map(|email| email.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
                || account.id.to_ascii_lowercase().contains(&needle)
        })
        .collect();

    match matches.len() {
        0 => bail!("找不到匹配的账号：{query}"),
        1 => Ok(matches[0]),
        count => bail!("有 {count} 个账号匹配「{query}」，请给出更完整的 ID 或邮箱"),
    }
}

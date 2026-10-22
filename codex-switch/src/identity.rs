//! 从 Codex 的 auth.json 里读出账号身份，纯本地解析，不联网。
//!
//! 依据与原作者一致：先看 `tokens.account_id`，没有就解 `tokens.id_token` 的
//! JWT payload，取 `email` 和 `https://api.openai.com/auth` 里的 `chatgpt_account_id`。

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub account_id: String,
    pub email: Option<String>,
    /// `chatgpt_plan_type`，例如 pro / plus / free / team。
    pub plan: Option<String>,
    /// `chatgpt_subscription_active_until` 的日期部分（YYYY-MM-DD）。
    pub active_until: Option<String>,
}

impl Identity {
    /// 给界面/命令行显示用的名字，拿不到邮箱时退化为打码后的 ID。
    #[cfg(test)]
    pub fn label(&self) -> String {
        match &self.email {
            Some(email) if !email.trim().is_empty() => email.clone(),
            _ => mask(&self.account_id),
        }
    }

    pub fn matches(&self, account_id: &str, email: Option<&str>) -> bool {
        if self.account_id != account_id {
            return false;
        }
        match (self.email.as_deref(), email) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => true,
        }
    }
}

pub fn mask(value: &str) -> String {
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(8).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        value.to_string()
    }
}

pub fn parse_auth_json(bytes: &[u8]) -> Result<Identity> {
    let bytes = strip_bom(bytes);
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("auth.json 不是合法 JSON")?;
    let tokens = value
        .get("tokens")
        .context("auth.json 里没有 tokens，可能不是 ChatGPT 登录格式")?;
    let claims = tokens
        .get("id_token")
        .and_then(|value| value.as_str())
        .and_then(jwt_payload);

    let auth = claims
        .as_ref()
        .and_then(|claims| claims.get("https://api.openai.com/auth"));

    let mut account_id = tokens
        .get("account_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    if account_id.is_none() {
        account_id = auth
            .and_then(|auth| auth.get("chatgpt_account_id"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
    }

    let account_id = account_id.context("凭证里找不到账号 ID")?;
    let email = claims
        .as_ref()
        .and_then(|claims| claims.get("email"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let plan = auth
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_ascii_lowercase());
    let active_until = auth
        .and_then(|auth| auth.get("chatgpt_subscription_active_until"))
        .and_then(|value| value.as_str())
        .and_then(date_part);

    Ok(Identity {
        account_id,
        email,
        plan,
        active_until,
    })
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}

fn date_part(value: &str) -> Option<String> {
    let date: String = value.chars().take(10).collect();
    if date.len() == 10 && date.as_bytes().get(4) == Some(&b'-') {
        Some(date)
    } else {
        None
    }
}

pub fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9";
    // {"email":"a@example.com","https://api.openai.com/auth":{"chatgpt_account_id":"acct-999"}}
    const CLAIMS: &str = "eyJlbWFpbCI6ImFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdC05OTkifX0";

    #[test]
    fn reads_account_id_from_tokens() {
        let json = br#"{"tokens":{"account_id":"acct-123"}}"#;
        let identity = parse_auth_json(json).unwrap();
        assert_eq!(identity.account_id, "acct-123");
        assert_eq!(identity.email, None);
    }

    #[test]
    fn falls_back_to_id_token_claims() {
        let token = format!("{HEADER}.{CLAIMS}.signature");
        let json = format!(r#"{{"tokens":{{"id_token":"{token}"}}}}"#);
        let identity = parse_auth_json(json.as_bytes()).unwrap();
        assert_eq!(identity.account_id, "acct-999");
        assert_eq!(identity.email.as_deref(), Some("a@example.com"));
        assert_eq!(identity.label(), "a@example.com");
    }

    #[test]
    fn rejects_files_without_tokens() {
        assert!(parse_auth_json(br#"{"OPENAI_API_KEY":"sk-test"}"#).is_err());
    }

    #[test]
    fn reads_plan_and_subscription_date() {
        use base64::Engine as _;
        let claims = serde_json::json!({
            "email": "b@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-1",
                "chatgpt_plan_type": "Pro",
                "chatgpt_subscription_active_until": "2026-11-20T12:00:00.000Z"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{HEADER}.{encoded}.signature");
        let json = format!(r#"{{"tokens":{{"id_token":"{token}"}}}}"#);
        let identity = parse_auth_json(json.as_bytes()).unwrap();
        assert_eq!(identity.plan.as_deref(), Some("pro"));
        assert_eq!(identity.active_until.as_deref(), Some("2026-11-20"));
    }
}

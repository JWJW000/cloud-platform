//! 管理后台会话令牌（JWT）。
//!
//! 角色值是中文（`超级管理员` / `任务管理员` / `只读用户`），与数据库一致，
//! 前端拿到后可直接展示，不需要再做一层翻译。

use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// 令牌声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 用户编号（UUID 字符串）。
    pub sub: String,
    /// 会话编号（UUID 字符串，对应 admin_sessions.id）。
    #[serde(default)]
    pub jti: String,
    /// 用户名。
    pub username: String,
    /// 中文角色名。
    pub role: String,
    /// 用户令牌版本。
    #[serde(default = "default_token_ver")]
    pub ver: i64,
    /// 过期时间（Unix 秒）。
    pub exp: i64,
    /// 签发时间（Unix 秒）。
    pub iat: i64,
}

fn default_token_ver() -> i64 {
    1
}

/// 令牌签发与校验。
#[derive(Clone)]
pub struct TokenIssuer {
    encoding: EncodingKey,
    decoding: DecodingKey,
    hours: i64,
}

impl std::fmt::Debug for TokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenIssuer(有效期 {} 小时)", self.hours)
    }
}

impl TokenIssuer {
    /// 由共享密钥构造。
    pub fn new(secret: &str, hours: i64) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            hours: hours.max(1),
        }
    }

    /// 获取配置的有效期小时数。
    pub fn validity_hours(&self) -> i64 {
        self.hours
    }

    /// 为用户签发令牌。
    pub fn issue(
        &self,
        user_id: &str,
        session_id: &str,
        username: &str,
        role: &str,
        ver: i64,
    ) -> Result<String> {
        let now = Utc::now();
        let claims = Claims {
            sub: user_id.to_string(),
            jti: session_id.to_string(),
            username: username.to_string(),
            role: role.to_string(),
            ver,
            exp: (now + Duration::hours(self.hours)).timestamp(),
            iat: now.timestamp(),
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|err| anyhow!("签发登录令牌失败：{err}"))
    }

    /// 校验令牌并返回声明。
    pub fn verify(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 5;
        decode::<Claims>(token, &self.decoding, &validation)
            .map(|data| data.claims)
            .map_err(|err| anyhow!("登录令牌无效或已过期：{err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn round_trip_keeps_chinese_role() {
        let issuer = TokenIssuer::new("0123456789abcdef", 2);
        let token = issuer
            .issue(
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222",
                "admin",
                "超级管理员",
                1,
            )
            .unwrap();
        let claims = issuer.verify(&token).unwrap();
        assert_eq!(claims.username, "admin");
        assert_eq!(claims.role, "超级管理员");
        assert_eq!(claims.ver, 1);
        assert_eq!(claims.jti, "22222222-2222-2222-2222-222222222222");
    }

    #[test]
    fn other_secret_cannot_verify() {
        let token = TokenIssuer::new("0123456789abcdef", 2)
            .issue("id", "jti-1", "admin", "超级管理员", 1)
            .unwrap();
        assert!(TokenIssuer::new("fedcba9876543210", 2)
            .verify(&token)
            .is_err());
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let issuer = TokenIssuer::new("0123456789abcdef", 2);
        let token = issuer.issue("id", "jti-1", "admin", "只读用户", 1).unwrap();
        // 替换载荷段：签名随即失效，不可能靠改角色提权
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Claims {
                sub: "id".to_string(),
                jti: "jti-1".to_string(),
                username: "admin".to_string(),
                role: "超级管理员".to_string(),
                ver: 1,
                exp: Utc::now().timestamp() + 3600,
                iat: Utc::now().timestamp(),
            })
            .unwrap(),
        );
        parts[1] = &forged_payload;
        assert!(issuer.verify(&parts.join(".")).is_err());
    }
}

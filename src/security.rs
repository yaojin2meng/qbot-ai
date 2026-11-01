use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use worker::*;

pub fn authorize(req: &Request, env: &Env) -> Result<Option<Response>> {
    let expected = env
        .secret("ADMIN_TOKEN")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if expected.len() < 32 {
        return Ok(Some(Response::error(
            "Configure ADMIN_TOKEN (at least 32 characters)",
            503,
        )?));
    }
    let auth = req.headers().get("Authorization")?.unwrap_or_default();
    let supplied = auth.strip_prefix("Bearer ").unwrap_or("");
    let a = Sha256::digest(supplied.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    if a.ct_eq(&b).unwrap_u8() != 1 {
        return Ok(Some(Response::error("Unauthorized", 401)?));
    }
    Ok(None)
}

/// A D1 conditional UPSERT is atomic; KV read-then-write is not suitable for a lock.
pub async fn acquire(env: &Env, key: &str, seconds: i64) -> Result<Option<String>> {
    let now = Date::now().as_millis() as i64;
    let owner = format!("{:032x}", rand::random::<u128>());
    let result = env
        .d1("DB")?
        .prepare(
            "INSERT INTO task_leases (lease_key, owner, expires_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(lease_key) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at
         WHERE task_leases.expires_at <= ?4",
        )
        .bind(&[
            key.into(),
            owner.clone().into(),
            (now + seconds * 1000).into(),
            now.into(),
        ])?
        .run()
        .await?;
    Ok(
        if result.meta()?.and_then(|m| m.changes).unwrap_or(0) == 1 {
            Some(owner)
        } else {
            None
        },
    )
}

pub async fn release(env: &Env, key: &str, owner: &str) -> Result<()> {
    env.d1("DB")?
        .prepare("DELETE FROM task_leases WHERE lease_key=?1 AND owner=?2")
        .bind(&[key.into(), owner.into()])?
        .run()
        .await?;
    Ok(())
}

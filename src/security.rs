use worker::*;

/// 分布式锁：基于 D1 原子 UPSERT 实现，用于去重和测速限流
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

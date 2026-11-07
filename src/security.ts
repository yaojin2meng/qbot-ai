import type { Env } from './env';

/**
 * 分布式锁：基于 D1 的原子 UPSERT 实现，用于事件去重和测速限流。
 * 只有 UPDATE 分支真正执行（changes === 1）才算拿到锁。
 */
export async function acquire(env: Env, key: string, seconds: number): Promise<string | null> {
  const now = Date.now();
  const owner = crypto.randomUUID();
  const result = await env.DB.prepare(
    `INSERT INTO task_leases (lease_key, owner, expires_at) VALUES (?1, ?2, ?3)
     ON CONFLICT(lease_key) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at
     WHERE task_leases.expires_at <= ?4`,
  )
    .bind(key, owner, now + seconds * 1000, now)
    .run();
  const changes = result?.meta?.changes ?? 0;
  return changes === 1 ? owner : null;
}

export async function release(env: Env, key: string, owner: string): Promise<void> {
  await env.DB.prepare('DELETE FROM task_leases WHERE lease_key=?1 AND owner=?2')
    .bind(key, owner)
    .run();
}
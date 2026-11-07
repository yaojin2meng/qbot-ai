import type { Env } from './env';
import {
  ADD_COLUMN_STATEMENTS,
  CREATE_STATEMENTS,
  REPAIR_STATEMENTS,
  SCHEMA_VERSION,
} from './schema';

const MARKER_KEY = 'schema_version';

/** 同一个 isolate 里只跑一次；跨 isolate 靠 settings 里的版本号判断。 */
let ready = false;
let inflight: Promise<void> | null = null;

/**
 * 确保表结构就绪。第一次请求时自动建表，不需要 `wrangler d1 migrations apply`。
 *
 * - 新库：一路 CREATE 过去，然后写版本号。
 * - 老库（跑过旧 migration）：CREATE 全部命中已存在，ALTER 报 duplicate column
 *   被忽略，最后把版本号更新掉。
 * - 并发：同 isolate 内复用同一个 Promise，多 isolate 同时跑也安全，
 *   因为所有 DDL 都是幂等的。
 */
export async function ensureSchema(env: Env): Promise<void> {
  if (ready) return;
  if (inflight) return inflight;
  inflight = init(env).finally(() => {
    inflight = null;
  });
  return inflight;
}

/** 只读探测：表结构是不是已经是最新版（不写库，供 /api/status 用）。 */
export async function schemaState(env: Env): Promise<{ version: number; ready: boolean }> {
  try {
    const row = await env.DB.prepare('SELECT value FROM settings WHERE key=?1')
      .bind(MARKER_KEY)
      .first<{ value: string }>();
    const version = Number(row?.value ?? 0);
    return { version: Number.isFinite(version) ? version : 0, ready: version >= SCHEMA_VERSION };
  } catch {
    return { version: 0, ready: false };
  }
}

async function init(env: Env): Promise<void> {
  if ((await schemaState(env)).ready) {
    ready = true;
    return;
  }

  // 顺序很重要：建表 → 补列 → 修数据。
  for (const sql of CREATE_STATEMENTS) await run(env, sql);
  for (const sql of ADD_COLUMN_STATEMENTS) await run(env, sql);
  for (const sql of REPAIR_STATEMENTS) await run(env, sql);

  await env.DB.prepare(
    `INSERT INTO settings (key, value, updated_at) VALUES (?1,?2,?3)
     ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at`,
  )
    .bind(MARKER_KEY, String(SCHEMA_VERSION), Date.now())
    .run();

  ready = true;
}

/** 已存在 / 重复列 这类「说明目标状态已达成」的报错直接吞掉。 */
function benign(error: unknown): boolean {
  const msg = String((error as Error)?.message ?? error).toLowerCase();
  return (
    msg.includes('already exists') ||
    msg.includes('duplicate column') ||
    msg.includes('duplicate key')
  );
}

async function run(env: Env, sql: string): Promise<void> {
  try {
    await env.DB.prepare(sql).run();
  } catch (e) {
    if (benign(e)) return;
    throw new Error(`执行 DDL 失败：${(e as Error).message}\n${sql.slice(0, 120)}`);
  }
}
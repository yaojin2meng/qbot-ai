import type { Env } from './env';

export const KEY_APP_ID = 'QQ_APP_ID';
export const KEY_APP_SECRET = 'QQ_APP_SECRET';
export const KEY_API_BASE = 'QQ_API_BASE';
export const KEY_SKIP_SIGNATURE = 'SKIP_SIGNATURE';
export const DEFAULT_API_BASE = 'https://api.sgroup.qq.com';
export const OFFICIAL_ORIGINS = ['https://api.sgroup.qq.com', 'https://api.bot.qq.com'];

export type Source = 'd1' | 'secret' | 'var' | 'missing';

/**
 * 读取一项配置，同时告诉调用方它从哪来（配置台要展示）。
 * 优先级：D1 settings 表 → wrangler secret / [vars] → 缺省。
 */
export async function lookup(env: Env, key: string): Promise<[string | null, Source]> {
  try {
    const row = await env.DB.prepare('SELECT value FROM settings WHERE key=?1')
      .bind(key)
      .first<{ value: string }>();
    if (row?.value && row.value.trim().length > 0) {
      return [row.value, 'd1'];
    }
  } catch {
    // settings 表可能还没建（0003 迁移未执行），此时静默回落到 env
  }
  // Workers 里 secret 和 var 在运行时都挂在 env 上，没法区分来源，
  // 这里按「敏感项标 secret，其余标 var」给前端一个可读的来源提示。
  const fromEnv = env[key];
  if (typeof fromEnv === 'string' && fromEnv.trim().length > 0) {
    return [fromEnv, key === KEY_APP_SECRET ? 'secret' : 'var'];
  }
  return [null, 'missing'];
}

export async function get(env: Env, key: string): Promise<string | null> {
  return (await lookup(env, key))[0];
}

export async function getOr(env: Env, key: string, fallback: string): Promise<string> {
  return (await get(env, key)) ?? fallback;
}

export async function flag(env: Env, key: string): Promise<boolean> {
  const value = await get(env, key);
  return value !== null && value.trim().toLowerCase() === 'true';
}

/** 写配置。空字符串表示删除该项，回落到 secret / vars / 默认值。 */
export async function set(env: Env, key: string, value: string): Promise<void> {
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    await env.DB.prepare('DELETE FROM settings WHERE key=?1').bind(key).run();
    return;
  }
  await env.DB.prepare(
    `INSERT INTO settings (key, value, updated_at) VALUES (?1,?2,?3)
     ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at`,
  )
    .bind(key, trimmed, Date.now())
    .run();
}

async function put(env: Env, key: string, value: string): Promise<string | null> {
  try {
    await set(env, key, value);
    return null;
  } catch (e) {
    return `写入 D1 失败（迁移 0003_settings.sql 是否已执行？）：${(e as Error).message}`;
  }
}

// ---------------------------------------------------------------------------
// 状态自检
// ---------------------------------------------------------------------------

interface Field {
  key: string;
  label: string;
  configured: boolean;
  source: Source;
  required: boolean;
  secret: boolean;
  value: string | null;
  hint: string;
}

interface FieldSpec {
  key: string;
  label: string;
  required: boolean;
  secret: boolean;
  hint: string;
}

const FIELD_SPECS: FieldSpec[] = [
  {
    key: KEY_APP_ID,
    label: 'QQ_APP_ID',
    required: true,
    secret: true,
    hint: 'QQ 开放平台 → 机器人 → 开发设置 → AppID',
  },
  {
    key: KEY_APP_SECRET,
    label: 'QQ_APP_SECRET',
    required: true,
    secret: true,
    hint: '同页面的 AppSecret。取 AccessToken 和回调验签都用它',
  },
  {
    key: KEY_API_BASE,
    label: 'QQ_API_BASE',
    required: false,
    secret: false,
    hint: '默认 api.sgroup.qq.com；遇到 403 换成 api.bot.qq.com',
  },
  {
    key: KEY_SKIP_SIGNATURE,
    label: 'SKIP_SIGNATURE',
    required: false,
    secret: false,
    hint: '只在本地调试时开；线上必须 false',
  },
];

export async function status(env: Env) {
  const fields: Field[] = [];
  for (const spec of FIELD_SPECS) {
    const [value, source] = await lookup(env, spec.key);
    fields.push({
      key: spec.key,
      label: spec.label,
      required: spec.required,
      secret: spec.secret,
      configured: value !== null,
      source,
      value: spec.secret ? null : value,
      hint: spec.hint,
    });
  }

  const setupRequired = fields
    .filter((f) => f.required)
    .some((f) => !f.configured);

  return {
    setup_required: setupRequired,
    fields,
    bindings: {
      db: Boolean(env.DB),
      kv: Boolean(env.KV),
      queue: Boolean(env.EVENTS),
    },
    webhook_path: '/qq/webhook',
    events: ['GROUP_AT_MESSAGE_CREATE', 'INTERACTION_CREATE'],
    permissions: [
      '群聊能力（群场景必需）',
      '主动消息',
      'Markdown 消息',
      '消息按钮（自定义按钮为内邀能力）',
    ],
    commands: ['投票 <主题> <人数>', '测速 <Base URL> <API Key>', '帮助'],
  };
}

// ---------------------------------------------------------------------------
// 首次初始化
// ---------------------------------------------------------------------------

interface SetupBody {
  app_id?: unknown;
  app_secret?: unknown;
  api_base?: unknown;
}

function asTrimmedString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export async function handleSetup(req: Request, env: Env): Promise<Response> {
  // 已经初始化过就不允许再写，避免被匿名请求覆盖配置。
  const [appIdNow] = await lookup(env, KEY_APP_ID);
  const [secretNow] = await lookup(env, KEY_APP_SECRET);
  if (appIdNow !== null && secretNow !== null) {
    return json({ ok: false, message: '已经初始化过了，无需重复配置。' });
  }

  let body: SetupBody;
  try {
    body = (await req.json()) as SetupBody;
  } catch {
    return json({ ok: false, message: '请求格式错误' });
  }

  const appId = asTrimmedString(body.app_id);
  const appSecret = asTrimmedString(body.app_secret);
  const apiBase = asTrimmedString(body.api_base);

  if (appId.length === 0 || appSecret.length === 0) {
    return json({ ok: false, message: 'QQ_APP_ID / QQ_APP_SECRET 都不能为空' });
  }
  if (apiBase.length > 0 && !checkOrigin(apiBase)) {
    return json({
      ok: false,
      message: `QQ_API_BASE 只能是 ${OFFICIAL_ORIGINS.join(' 或 ')} 之一`,
    });
  }

  for (const [key, value] of [
    [KEY_APP_ID, appId],
    [KEY_APP_SECRET, appSecret],
  ] as const) {
    const error = await put(env, key, value);
    if (error) return json({ ok: false, message: error });
  }
  if (apiBase.length > 0) {
    const error = await put(env, KEY_API_BASE, apiBase);
    if (error) return json({ ok: false, message: error });
  }

  return json({ ok: true, message: '初始化完成' });
}

function checkOrigin(base: string): boolean {
  const trimmed = base.trim().replace(/\/+$/, '');
  return OFFICIAL_ORIGINS.includes(trimmed);
}

// ---------------------------------------------------------------------------
// 一键验证凭据
// ---------------------------------------------------------------------------

export async function handleVerify(_req: Request, env: Env): Promise<Response> {
  const { probe } = await import('./qqApi');
  try {
    await probe(env);
    return json({ ok: true, message: '凭据可用，已成功拿到 QQ AccessToken' });
  } catch (e) {
    return json({ ok: false, message: `拿不到 AccessToken：${(e as Error).message}` });
  }
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' },
  });
}
import * as config from './config';
import type { Env } from './env';
import { fetchJson } from './http';

/**
 * 取 AccessToken 的地址。
 *
 * 官方文档（API 调用指南）写的是 `https://api.bot.qq.com/app/getAppAccessToken`，
 * 并说明「统一地址 https://api.bot.qq.com」。但官方 SDK（botpy/botgo）与早期
 * 文档一直用 `https://bots.qq.com/app/getAppAccessToken`，两个域名目前都还有在用。
 * 所以按文档地址优先、老域名兜底，避免任何一个域名下线就整条链路挂掉。
 */
const TOKEN_ENDPOINTS = [
  'https://api.bot.qq.com/app/getAppAccessToken',
  'https://bots.qq.com/app/getAppAccessToken',
] as const;

interface TokenResponse {
  status: number;
  json: unknown;
}

/** 依次尝试各端点；第一个能拿到 200 响应的就用它。 */
async function requestToken(appId: string, secret: string): Promise<TokenResponse> {
  let lastError: unknown = null;
  for (const endpoint of TOKEN_ENDPOINTS) {
    try {
      const result = await fetchJson(
        endpoint,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ appId, clientSecret: secret }),
        },
        8,
      );
      if (result.status === 200) return result;
      lastError = new Error(`${endpoint} 返回 HTTP ${result.status}`);
    } catch (e) {
      lastError = e;
    }
    console.error('access token endpoint failed, trying next:', String(lastError));
  }
  throw new Error(
    `取 AccessToken 失败：${(lastError as Error)?.message ?? '所有端点均不可用'}`,
  );
}

export function validId(value: string): boolean {
  return (
    value.length > 0 &&
    value.length <= 256 &&
    /^[A-Za-z0-9_-]+$/.test(value)
  );
}

async function apiOrigin(env: Env): Promise<string> {
  const origin = (await config.getOr(env, config.KEY_API_BASE, config.DEFAULT_API_BASE))
    .trim()
    .replace(/\/+$/, '');
  if (!config.OFFICIAL_ORIGINS.includes(origin)) {
    throw new Error('QQ_API_BASE 必须是 api.sgroup.qq.com 或 api.bot.qq.com');
  }
  return origin;
}

function readCode(value: unknown): number {
  if (typeof value !== 'object' || value === null) return 0;
  const code = (value as Record<string, unknown>).code;
  if (typeof code === 'number') return code;
  if (typeof code === 'string') {
    const parsed = Number.parseInt(code, 10);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

export async function request(
  env: Env,
  method: string,
  path: string,
  body: unknown,
): Promise<unknown> {
  const token = await getAccessToken(env);
  const url = `${await apiOrigin(env)}${path}`;
  const { status, json } = await fetchJson(
    url,
    {
      method,
      headers: {
        Authorization: `QQBot ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    },
    10,
  );

  if (status === 401) {
    await invalidateToken(env).catch((e) =>
      console.error('could not invalidate QQ token cache:', e),
    );
  }

  const code = readCode(json);
  if (status < 200 || status >= 300 || code !== 0) {
    // 不回显上游响应体：里面可能带凭据或用户内容。
    throw new Error(`QQ API rejected request (HTTP ${status}, code ${code})`);
  }
  return json;
}

export async function sendGroup(env: Env, groupId: string, payload: unknown): Promise<void> {
  if (!validId(groupId)) throw new Error('invalid group id');
  await request(env, 'POST', `/v2/groups/${groupId}/messages`, payload);
}

export async function ackInteraction(env: Env, id: string): Promise<void> {
  if (!validId(id)) throw new Error('invalid interaction id');
  await request(env, 'PUT', `/interactions/${id}`, { code: 0 });
}

async function getAccessToken(env: Env): Promise<string> {
  const appId = await config.get(env, config.KEY_APP_ID);
  const secret = await config.get(env, config.KEY_APP_SECRET);
  if (!appId || !secret) {
    throw new Error('未配置 QQ_APP_ID / QQ_APP_SECRET，请打开网页配置台填写');
  }
  if (appId.trim().length === 0 || secret.length === 0) {
    throw new Error('QQ 凭据为空，请打开网页配置台填写');
  }

  const cacheKey = `qq_access_token:${appId}`;
  const cached = await env.KV.get(cacheKey);
  if (cached) return cached;

  const { status, json } = await requestToken(appId, secret);
  if (status !== 200) {
    throw new Error(
      `QQ 取 AccessToken 失败（HTTP ${status}），请检查 QQ_APP_ID / QQ_APP_SECRET`,
    );
  }

  const token = readString(json, 'access_token');
  if (!token) {
    throw new Error('QQ 返回里没有 access_token，请检查 QQ_APP_ID / QQ_APP_SECRET');
  }
  const expires = readNumber(json, 'expires_in');
  if (expires === null) {
    throw new Error('QQ token endpoint returned invalid expires_in');
  }

  // KV 的最小 TTL 是 60 秒，剩余寿命太短的 token 不缓存。
  if (expires >= 120) {
    await env.KV.put(cacheKey, token, { expirationTtl: Math.floor(expires - 60) }).catch((e) =>
      console.error('QQ token cache write failed:', e),
    );
  }
  return token;
}

async function invalidateToken(env: Env): Promise<void> {
  const appId = await config.get(env, config.KEY_APP_ID);
  if (!appId) return;
  await env.KV.delete(`qq_access_token:${appId}`);
}

/**
 * 配置台用：跳过缓存直连上游取一次 token，验证 AppID / AppSecret 是否可用。
 */
export async function probe(env: Env): Promise<void> {
  const appId = await config.get(env, config.KEY_APP_ID);
  if (!appId) throw new Error('还没有填写 QQ_APP_ID');
  const secret = await config.get(env, config.KEY_APP_SECRET);
  if (!secret) throw new Error('还没有填写 QQ_APP_SECRET');
  if (appId.trim().length === 0 || secret.length === 0) {
    throw new Error('QQ_APP_ID / QQ_APP_SECRET 不能为空');
  }

  const { status, json } = await requestToken(appId, secret);
  if (status !== 200) {
    throw new Error(`QQ 返回 HTTP ${status}`);
  }
  if (readString(json, 'access_token')) {
    // 验证通过顺手清掉旧缓存，省得下次还用过期 token。
    await invalidateToken(env);
    return;
  }
  throw new Error(
    readString(json, 'message') ?? readString(json, 'msg') ?? '返回里没有 access_token',
  );
}

function readString(value: unknown, key: string): string | null {
  if (typeof value !== 'object' || value === null) return null;
  const field = (value as Record<string, unknown>)[key];
  return typeof field === 'string' && field.length > 0 ? field : null;
}

function readNumber(value: unknown, key: string): number | null {
  if (typeof value !== 'object' || value === null) return null;
  const field = (value as Record<string, unknown>)[key];
  if (typeof field === 'number' && Number.isFinite(field)) return field;
  if (typeof field === 'string') {
    const parsed = Number.parseInt(field, 10);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}
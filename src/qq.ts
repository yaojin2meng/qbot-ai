import * as config from './config';
import type { Env } from './env';
import * as modelTest from './modelTest';
import * as poll from './poll';
import { ackInteraction, sendGroup, validId } from './qqApi';
import * as security from './security';

const OP_DISPATCH = 0;
const OP_ACK = 12;
const OP_VERIFY = 13;
const MAX_BODY_BYTES = 64 * 1024;
const SIGNATURE_WINDOW_SECONDS = 300;

export interface GroupAtMessage {
  id: string;
  content: string;
  group_openid: string;
  author?: { id?: string; member_openid?: string };
}

export interface InteractionCreate {
  id: string;
  type?: number;
  group_openid?: string;
  group_member_openid?: string;
  user_openid?: string;
  data?: {
    type?: number;
    resolved?: { button_data?: string; button_id?: string };
  };
}

export type EventJob =
  | { kind: 'group'; event: GroupAtMessage }
  | { kind: 'interaction'; event: InteractionCreate; event_id: string };

interface Payload {
  op?: number;
  d?: unknown;
  id?: string;
  s?: number;
  t?: string;
}

// ---------------------------------------------------------------------------
// Ed25519（WebCrypto）
// ---------------------------------------------------------------------------

/** PKCS#8 v1 里 Ed25519 私钥的固定前缀，后面接 32 字节 seed */
const PKCS8_ED25519_PREFIX = new Uint8Array([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
  0x20,
]);

/**
 * QQ 的约定：把 AppSecret 反复填充到 32 字节当作 Ed25519 seed。
 * 这和官方 DEMO 完全一致，别改成哈希。
 */
function buildSeed(secret: string): Uint8Array {
  const secretBytes = new TextEncoder().encode(secret);
  if (secretBytes.length === 0) throw new Error('QQ_APP_SECRET must not be empty');
  const seed = new Uint8Array(32);
  for (let i = 0; i < seed.length; i += 1) {
    seed[i] = secretBytes[i % secretBytes.length] as number;
  }
  return seed;
}

async function importSigningKey(secret: string): Promise<CryptoKey> {
  const seed = buildSeed(secret);
  const der = new Uint8Array(PKCS8_ED25519_PREFIX.length + seed.length);
  der.set(PKCS8_ED25519_PREFIX);
  der.set(seed, PKCS8_ED25519_PREFIX.length);
  return crypto.subtle.importKey('pkcs8', der, { name: 'Ed25519' }, false, ['sign']);
}

function toHex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Ed25519 是确定性签名：用同一个 seed 对同样的报文再签一次，
 * 和请求头里的签名逐字节比对，等价于验签。
 */
async function signWithSecret(secret: string, data: string): Promise<string> {
  const key = await importSigningKey(secret);
  const signature = await crypto.subtle.sign(
    { name: 'Ed25519' },
    key,
    new TextEncoder().encode(data),
  );
  return toHex(new Uint8Array(signature));
}

function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }
  return diff === 0;
}

function timestampIsFresh(timestamp: string, nowSeconds: number): boolean {
  const parsed = Number.parseInt(timestamp, 10);
  if (!Number.isFinite(parsed)) return false;
  return Math.abs(nowSeconds - parsed) <= SIGNATURE_WINDOW_SECONDS;
}

async function verifySignature(
  env: Env,
  signatureHex: string,
  timestamp: string,
  body: string,
): Promise<boolean> {
  const secret = await config.get(env, config.KEY_APP_SECRET);
  if (!secret) return false;
  if (!/^[0-9a-fA-F]{128}$/.test(signatureHex)) return false;
  const expected = await signWithSecret(secret, `${timestamp}${body}`);
  return timingSafeEqual(expected, signatureHex.toLowerCase());
}

// ---------------------------------------------------------------------------
// Webhook
// ---------------------------------------------------------------------------

export async function handleWebhook(req: Request, env: Env): Promise<Response> {
  const signatureHex = req.headers.get('X-Signature-Ed25519') ?? '';
  const timestamp = req.headers.get('X-Signature-Timestamp') ?? '';
  const body = await req.text();
  if (body.length > MAX_BODY_BYTES) {
    return new Response('payload too large', { status: 413 });
  }

  const skip = await config.flag(env, config.KEY_SKIP_SIGNATURE);
  if (!skip) {
    const fresh = timestampIsFresh(timestamp, Math.floor(Date.now() / 1000));
    if (!fresh || !(await verifySignature(env, signatureHex, timestamp, body))) {
      return new Response('invalid or expired signature', { status: 401 });
    }
  } else {
    console.error('WARNING: QQ signature verification is disabled');
  }

  let payload: Payload;
  try {
    payload = JSON.parse(body) as Payload;
  } catch {
    return new Response('bad payload', { status: 400 });
  }

  // 回调地址验证
  if (payload.op === OP_VERIFY) {
    const data = (payload.d ?? {}) as { plain_token?: unknown; event_ts?: unknown };
    const plainToken = typeof data.plain_token === 'string' ? data.plain_token : '';
    const eventTs = typeof data.event_ts === 'string' ? data.event_ts : '';
    if (plainToken.length === 0 || eventTs.length === 0) {
      return new Response('invalid challenge', { status: 400 });
    }
    const secret = await config.get(env, config.KEY_APP_SECRET);
    if (!secret) {
      return new Response('QQ_APP_SECRET not configured', { status: 500 });
    }
    const signature = await signWithSecret(secret, `${eventTs}${plainToken}`);
    return json({ plain_token: plainToken, signature });
  }

  if (payload.op === OP_DISPATCH) {
    const job = await buildJob(payload, env);
    if (job instanceof Response) return job;
    if (job) {
      // 先入队再 ACK：入队失败必须让 QQ 重推，不能假装处理成功。
      try {
        await env.EVENTS.send(job);
      } catch (e) {
        console.error('QQ event enqueue failed:', e);
        return new Response('queue unavailable', { status: 503 });
      }
    }
  }

  return json({ op: OP_ACK });
}

/** 返回 undefined 表示「不需要处理」，返回 Response 表示「直接按错误回包」 */
async function buildJob(payload: Payload, env: Env): Promise<EventJob | Response | undefined> {
  const data = payload.d ?? {};
  const eventType = payload.t ?? '';

  if (eventType === 'GROUP_AT_MESSAGE_CREATE') {
    const event = data as GroupAtMessage;
    if (
      typeof event.id !== 'string' ||
      typeof event.group_openid !== 'string' ||
      typeof event.content !== 'string' ||
      !validId(event.id) ||
      !validId(event.group_openid)
    ) {
      return new Response('invalid group event', { status: 400 });
    }
    return { kind: 'group', event };
  }

  if (eventType === 'INTERACTION_CREATE') {
    const event = data as InteractionCreate;
    if (typeof event.id !== 'string' || !validId(event.id)) {
      return new Response('invalid interaction id', { status: 400 });
    }
    const kind = typeof event.type === 'number' ? event.type : 0;
    if (kind === 11 || kind === 12) {
      try {
        await ackInteraction(env, event.id);
      } catch (e) {
        // 重复投递时 QQ 会说「已经回应过」，这时仍要继续走业务逻辑。
        console.error('interaction acknowledgement failed:', e);
      }
    }
    if (kind !== 11) return undefined;
    const eventId = payload.id ?? '';
    if (!validId(eventId)) {
      return new Response('missing outer event id', { status: 400 });
    }
    return { kind: 'interaction', event, event_id: eventId };
  }

  return undefined;
}

// ---------------------------------------------------------------------------
// 队列消费（幂等）
// ---------------------------------------------------------------------------

export async function processJob(job: EventJob, env: Env): Promise<void> {
  const key =
    job.kind === 'group'
      ? `gmsg:${job.event.group_openid}:${job.event.id}`
      : `intr:${job.event.id}`;

  if (await seen(env, key)) return;

  const owner = await security.acquire(env, key, 600);
  if (!owner) throw new Error('event is already processing');

  try {
    if (await seen(env, key)) return;

    if (job.kind === 'group') {
      await handleGroupMessage(job.event, env);
    } else {
      await handleInteraction(job.event, job.event_id, env);
    }

    await env.DB.prepare(
      'INSERT OR IGNORE INTO webhook_events (event_key, completed_at) VALUES (?1, ?2)',
    )
      .bind(key, Date.now())
      .run();

    // 1/32 概率顺手清理过期数据，避免表无限增长
    if (Math.floor(Math.random() * 32) === 0) {
      await env.DB.prepare('DELETE FROM webhook_events WHERE completed_at < ?1')
        .bind(Date.now() - 7 * 86400000)
        .run();
      await env.DB.prepare('DELETE FROM task_leases WHERE expires_at < ?1')
        .bind(Date.now() - 86400000)
        .run();
    }
  } finally {
    await security.release(env, key, owner).catch((e) =>
      console.error('event lease release failed:', e),
    );
  }
}

async function seen(env: Env, key: string): Promise<boolean> {
  const row = await env.DB.prepare('SELECT event_key FROM webhook_events WHERE event_key=?1')
    .bind(key)
    .first<{ event_key: string }>();
  return row !== null;
}

// ---------------------------------------------------------------------------
// 业务分发
// ---------------------------------------------------------------------------

function stripMention(content: string): string {
  const trimmed = content.trim();
  if (trimmed.startsWith('<@')) {
    const end = trimmed.indexOf('>');
    if (end !== -1) return trimmed.slice(end + 1).trim();
  }
  return trimmed;
}

async function handleGroupMessage(event: GroupAtMessage, env: Env): Promise<void> {
  const text = stripMention(event.content);
  if (text.length === 0) return;

  if (['帮助', 'help', '?', '？'].includes(text)) {
    await sendHelp(env, event);
    return;
  }

  const vote = poll.parseVoteCommand(text);
  if (vote) {
    await poll.createPoll(env, event.group_openid, event.id, vote.topic, vote.count);
    return;
  }

  // 只有以「测速」开头的消息才当测速指令，避免群里随口贴个链接就触发。
  if (text.startsWith('测速') && /^\s/.test(text.slice(2))) {
    const rest = text.slice(2).trim();
    const parsed = modelTest.extractApiAndKey(rest);
    if (!parsed) {
      await sendGroup(env, event.group_openid, {
        msg_type: 0,
        content:
          '没认出 API 地址或 Key。格式：测速 <Base URL> <API Key>，例如\n测速 https://api.deepseek.com/v1 sk-xxxx',
        msg_id: event.id,
        msg_seq: 1,
      });
      return;
    }

    const leaseKey = `group-test:${event.group_openid}`;
    const owner = await security.acquire(env, leaseKey, 600);
    if (!owner) {
      await sendGroup(env, event.group_openid, {
        msg_type: 0,
        content: '本群测速正在进行或处于冷却期，请 10 分钟后重试。',
        msg_id: event.id,
        msg_seq: 1,
      });
      return;
    }
    try {
      await modelTest.testAndReplyGroup(env, event.group_openid, event.id, parsed.base, parsed.key);
    } catch (e) {
      await security.release(env, leaseKey, owner);
      throw e;
    }
  }
}

async function sendHelp(env: Env, event: GroupAtMessage): Promise<void> {
  const content =
    '# 🤖 可用指令\n\n' +
    '**投票** `<主题> <人数>`\n' +
    '> 例：`投票 周末去爬山吗 10`\n' +
    '> 发出带三个按钮的卡片，每人一票，达到人数自动结算。\n\n' +
    '**测速** `<Base URL> <API Key>`\n' +
    '> 例：`测速 https://api.deepseek.com/v1 sk-xxxx`\n' +
    '> 最多测 30 个模型，结果以文本报告发回群。\n\n' +
    '**帮助**\n' +
    '> 就是这条消息。';
  await sendGroup(env, event.group_openid, {
    msg_type: 2,
    markdown: { content },
    msg_id: event.id,
    msg_seq: 1,
  });
}

async function handleInteraction(
  event: InteractionCreate,
  eventId: string,
  env: Env,
): Promise<void> {
  const user = event.group_member_openid ?? event.user_openid ?? '';
  const group = event.group_openid ?? '';
  if (!validId(user) || !validId(group)) return;

  const buttonData = event.data?.resolved?.button_data ?? '';
  await poll.handleVoteClick(env, group, user, buttonData, eventId);
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json; charset=utf-8' },
  });
}
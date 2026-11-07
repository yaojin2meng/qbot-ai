import { fetchJson } from './http';
import type { Env } from './env';
import { sendGroup } from './qqApi';

const MAX_MODELS = 30;
const CONCURRENCY = 5;
const REQUEST_TIMEOUT_SECONDS = 15;
/** QQ 单条文本消息的安全长度，超出会截断 */
const MAX_REPORT_CHARS = 3000;

export interface ModelResult {
  model: string;
  ok: boolean;
  latency_ms: number | null;
  error: string | null;
  prompt: string;
  answer: string;
}

const BLOCKED_SUFFIXES = ['.localhost', '.local', '.internal', '.test', '.invalid'];

function isIpLiteral(host: string): boolean {
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(host)) return true;
  return host.includes(':') || host.startsWith('[');
}

/**
 * 裸域名补 `/v1`；显式写了版本/自定义路径就保留。
 * 只允许公网 HTTPS，禁止字面量 IP、userinfo、query/fragment 和本地域名。
 */
export function normalizeBaseUrl(input: string): string {
  const trimmed = input.trim();
  if (trimmed.length > 2048) throw new Error('API URL too long');

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new Error('invalid API URL');
  }

  const host = url.hostname.toLowerCase();
  const badPort = url.port !== '' && url.port !== '443';
  if (
    url.protocol !== 'https:' ||
    url.username.length > 0 ||
    url.password.length > 0 ||
    url.search.length > 0 ||
    url.hash.length > 0 ||
    badPort ||
    isIpLiteral(host) ||
    !host.includes('.') ||
    host.endsWith('.') ||
    BLOCKED_SUFFIXES.some((suffix) => host.endsWith(suffix))
  ) {
    throw new Error('only public HTTPS API origins without credentials are allowed');
  }

  const path = url.pathname.replace(/\/+$/, '');
  url.pathname = path.length === 0 ? '/v1' : path;
  return url.toString().replace(/\/+$/, '');
}

function configPair(baseUrl: string, key: string): { base: string; key: string } | null {
  const trimmedKey = key.trim();
  if (trimmedKey.length === 0 || trimmedKey.length > 4096) return null;
  if (/[\u0000-\u001f\u007f]/.test(trimmedKey)) return null;
  try {
    return { base: normalizeBaseUrl(baseUrl), key: trimmedKey };
  } catch {
    return null;
  }
}

/**
 * 判断一段文本是不是「连接信息」形态，用于在群里直接粘配置时自动触发测速。
 *
 * 认两种：
 *   1) JSON：`{"key":"sk-…","url":"https://…"}`（new-api / one-api 的连接信息）
 *   2) 纯文本：一条 http(s) 地址 + 一个 sk- 开头的长 key，顺序不限，
 *      中间换行/空格都行，且整段不超过 4 个 token（防止把聊天正文当配置）。
 *
 * 不认：普通链接、只提到 sk- 的讨论、缺 url 或 key 的 JSON。
 */
export function looksLikeConfigPayload(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length === 0 || trimmed.length > 4096) return false;

  // 1) JSON 形态
  if (
    trimmed.includes('{') &&
    trimmed.includes('}') &&
    /"(?:key|api_key)"\s*:/.test(trimmed) &&
    /"(?:url|base_url)"\s*:/.test(trimmed)
  ) {
    return true;
  }

  // 2) 纯文本形态
  const tokens = trimmed.split(/\s+/).filter((t) => t.length > 0);
  if (tokens.length === 0 || tokens.length > 4) return false;

  const isUrl = (t: string) => /^https?:\/\//i.test(t) && t.length > 10;
  const isKey = (t: string) => /^sk-[A-Za-z0-9_-]{16,}$/.test(t);

  return tokens.filter(isUrl).length === 1 && tokens.filter(isKey).length === 1;
}

/**
 * 支持三种写法：JSON、两行带标签、或空格分隔的 URL + Key。
 */
export function extractApiAndKey(text: string): { base: string; key: string } | null {
  const trimmed = text.trim();

  const open = trimmed.indexOf('{');
  const close = trimmed.lastIndexOf('}');
  if (open !== -1 && close > open) {
    try {
      const parsed = JSON.parse(trimmed.slice(open, close + 1)) as Record<string, unknown>;
      const url = [parsed['url'], parsed['base_url']].find((v) => typeof v === 'string') as
        | string
        | undefined;
      const key = [parsed['key'], parsed['api_key']].find((v) => typeof v === 'string') as
        | string
        | undefined;
      if (url && key) {
        const pair = configPair(url, key);
        if (pair) return pair;
      }
    } catch {
      // 不是合法 JSON，继续走后面的解析
    }
  }

  const parts = trimmed.split(/\s+/).filter((p) => p.length > 0);
  if (parts.length === 2) {
    const [first, second] = parts as [string, string];
    if (first.startsWith('https://')) return configPair(first, second);
    if (second.startsWith('https://')) return configPair(second, first);
  }

  let base: string | null = null;
  let key: string | null = null;
  const LABELS = ['url:', 'url=', 'base_url:', 'base_url=', 'key:', 'key=', 'api_key:', 'api_key='];
  for (const rawLine of trimmed.split('\n')) {
    const line = rawLine.trim();
    if (line.length === 0) continue;
    const lower = line.toLowerCase();
    let value = line;
    let keyLabel = false;
    for (const label of LABELS) {
      if (lower.startsWith(label)) {
        value = line.slice(label.length).trim();
        keyLabel = label.startsWith('key') || label.startsWith('api_key');
        break;
      }
    }
    if (value.startsWith('https://') && base === null) {
      base = value;
    } else if ((keyLabel || value.startsWith('sk-')) && !/\s/.test(value) && key === null) {
      key = value;
    }
  }
  return base && key ? configPair(base, key) : null;
}

export function truncate(text: string, limit: number): string {
  const chars = [...text];
  return chars.length <= limit ? text : `${chars.slice(0, limit).join('')}…`;
}

function randomPrompt(): string {
  const kind = Math.floor(Math.random() * 3);
  const rand = (max: number, min = 0) => Math.floor(Math.random() * max) + min;
  if (kind === 0) {
    const a = rand(900, 100);
    const b = rand(90, 10);
    const c = rand(500, 10);
    return `请只输出计算结果，不要解释：${a} * ${b} + ${c} =`;
  }
  if (kind === 1) {
    const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
    let s = '';
    for (let i = 0; i < 8; i += 1) s += chars[Math.floor(Math.random() * chars.length)];
    return `将字符串 '${s}' 完全逆序输出，只输出逆序结果：`;
  }
  const nums = Array.from({ length: 5 }, () => rand(100));
  return `请只输出以下数字之和的阿拉伯数字结果：${nums.join(' + ')}`;
}

async function fetchModelList(base: string, key: string): Promise<string[]> {
  const { status, json } = await fetchJson(
    `${base}/models`,
    { method: 'GET', headers: { Authorization: `Bearer ${key}` } },
    REQUEST_TIMEOUT_SECONDS,
  );
  if (status !== 200) throw new Error(`model list HTTP ${status}`);
  const data = (json as { data?: unknown } | null)?.data;
  if (!Array.isArray(data)) throw new Error('invalid model list');

  const models = data
    .map((item) => (item as { id?: unknown })?.id)
    .filter((id): id is string => typeof id === 'string' && id.length > 0 && id.length <= 200)
    .slice(0, 1000);
  return [...new Set(models)].sort();
}

function fail(model: string, prompt: string, error: string): ModelResult {
  return { model, ok: false, latency_ms: null, error, prompt, answer: '' };
}

async function testOne(base: string, key: string, model: string): Promise<ModelResult> {
  const prompt = randomPrompt();
  const startedAt = Date.now();
  let result: { status: number; json: unknown };
  try {
    result = await fetchJson(
      `${base}/chat/completions`,
      {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${key}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          model,
          messages: [{ role: 'user', content: prompt }],
          temperature: 0.2,
          max_tokens: 64,
          stream: false,
        }),
      },
      REQUEST_TIMEOUT_SECONDS,
    );
  } catch {
    return fail(model, prompt, '请求失败/超时/非 JSON 响应');
  }

  if (result.status !== 200) return fail(model, prompt, `HTTP ${result.status}`);

  const content = readContent(result.json);
  if (content.length === 0) return fail(model, prompt, '空回（返回内容为空）');

  return {
    model,
    ok: true,
    latency_ms: Date.now() - startedAt,
    error: null,
    prompt,
    answer: truncate(content, 120).replace(key, '[REDACTED]'),
  };
}

function readContent(value: unknown): string {
  const choices = (value as { choices?: unknown } | null)?.choices;
  if (!Array.isArray(choices)) return '';
  const first = choices[0] as { message?: { content?: unknown } } | undefined;
  const content = first?.message?.content;
  return typeof content === 'string' ? content.trim() : '';
}

async function testModels(
  base: string,
  key: string,
  models: string[],
): Promise<ModelResult[]> {
  const picked = models.slice(0, MAX_MODELS);
  const results: ModelResult[] = new Array(picked.length);
  let cursor = 0;

  const worker = async () => {
    for (;;) {
      const index = cursor;
      cursor += 1;
      if (index >= picked.length) return;
      const model = picked[index];
      if (model === undefined) continue;
      results[index] = await testOne(base, key, model);
    }
  };

  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, picked.length) }, worker));
  return results.filter((r): r is ModelResult => r !== undefined);
}

function formatReport(results: ModelResult[]): string {
  const okCount = results.filter((r) => r.ok).length;
  const lines = [
    `模型测速报告（成功 ${okCount}/${results.length}，单位 ms）`,
    '',
  ];
  for (const r of results) {
    const detail = r.ok ? `${r.latency_ms ?? 0} ms` : (r.error ?? '失败');
    lines.push(`${truncate(r.model, 60)}: ${detail}`);
  }
  return truncate(lines.join('\n'), MAX_REPORT_CHARS);
}

/**
 * Markdown 版报告。只用 QQ Markdown 支持的语法（加粗、列表、引用），
 * 不写表格 —— QQ 客户端对表格支持不一致。
 */
function formatReportMarkdown(results: ModelResult[]): string {
  const okCount = results.filter((r) => r.ok).length;
  const lines = [
    '**模型测速报告**',
    '',
    `成功 **${okCount}/${results.length}**，单位 ms`,
    '',
  ];
  for (const r of results) {
    const detail = r.ok ? `${r.latency_ms ?? 0} ms` : `❌ ${r.error ?? '失败'}`;
    lines.push(`- ${truncate(r.model, 60)}: ${detail}`);
  }
  return truncate(lines.join('\n'), MAX_REPORT_CHARS);
}

/**
 * 发消息但永不抛错：投递失败只记日志。
 *
 * 测速本身已经花掉了上游额度，如果仅仅因为消息没发出去（比如超出 QQ 的
 * 5 分钟被动回复窗口）就让整个队列 job 失败，cloudflare 会重试 3 次，
 * 结果是同一批模型被反复重测、额度白白烧掉。所以投递失败一律吞掉。
 */
async function safeSend(env: Env, group: string, payload: unknown): Promise<void> {
  try {
    await sendGroup(env, group, payload);
  } catch (e) {
    console.error('QQ message delivery failed:', e);
  }
}

/**
 * 发最终报告：优先「主动消息 + Markdown」，失败回退「被动回复 + 纯文本」。
 *
 * 为什么用主动消息：主动消息不带 msg_id/msg_seq，不受 QQ「被动回复必须 5 分钟内、
 * 同一条最多 5 次」的限制。测速最长要 90 秒，加上队列延迟，被动回复很容易超窗，
 * 超了报告就发不出去。主动消息的代价是走配额（Bot 维度 60 qpm、单群 20 qpm /
 * 每天 1000 条），一次测速只用 1 条，完全够。
 *
 * 回退路径保证「万一没开 Markdown 权限或配额耗尽」，用户至少还能拿到纯文本结果。
 */
async function sendReport(
  env: Env,
  group: string,
  msg: string,
  results: ModelResult[],
): Promise<void> {
  try {
    await sendGroup(env, group, {
      msg_type: 2,
      markdown: { content: formatReportMarkdown(results) },
    });
    return;
  } catch (e) {
    console.error('markdown report delivery failed, falling back to plain text:', e);
  }
  await safeSend(env, group, {
    msg_type: 0,
    content: formatReport(results),
    msg_id: msg,
    msg_seq: 3,
  });
}

export async function testAndReplyGroup(
  env: Env,
  group: string,
  msg: string,
  base: string,
  key: string,
): Promise<void> {
  let normalized: string;
  try {
    normalized = normalizeBaseUrl(base);
  } catch (e) {
    await safeSend(env, group, {
      msg_type: 0,
      content: `API Base URL 不合法：${(e as Error).message}`,
      msg_id: msg,
      msg_seq: 1,
    });
    return;
  }

  await safeSend(env, group, {
    msg_type: 0,
    content: '已识别 API，正在测速（最多 30 个模型，单个超时 15 秒）。',
    msg_id: msg,
    msg_seq: 1,
  });

  let models: string[];
  try {
    models = await fetchModelList(normalized, key);
  } catch (e) {
    console.error('fetch model list failed:', e);
    models = [];
  }
  if (models.length === 0) {
    await safeSend(env, group, {
      msg_type: 0,
      content: '拉取模型失败或列表为空，请检查 API Base URL、Key 与接口权限。',
      msg_id: msg,
      msg_seq: 2,
    });
    return;
  }

  const results = await testModels(normalized, key, models);
  await sendReport(env, group, msg, results);
}
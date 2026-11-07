/** 上游 JSON 响应体上限，防止被超大响应拖死 */
export const MAX_BODY_BYTES = 2 * 1024 * 1024;

export interface FetchResult {
  status: number;
  json: unknown;
}

/**
 * 带整体超时 + 体积上限的 JSON 请求。
 * 超时覆盖「拿到响应头」和「读完响应体」两段，和原来 Rust 版语义一致。
 */
export async function fetchJson(
  url: string,
  init: RequestInit,
  timeoutSeconds = 10,
): Promise<FetchResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutSeconds * 1000);
  try {
    // Workers 的 fetch 只认 'follow' 和 'manual'，传 'error' 会直接抛
    // "Invalid redirect value"。这里用 manual，然后自己把 3xx 当失败，
    // 语义等于 Node/undici 的 redirect:'error'。
    const resp = await fetch(url, {
      ...init,
      redirect: 'manual',
      signal: controller.signal,
    });
    const status = resp.status;
    if (status >= 300 && status < 400) {
      await resp.body?.cancel();
      throw new Error(`upstream returned HTTP ${status} (redirect not followed)`);
    }
    if (status === 204 || status === 205) {
      return { status, json: null };
    }
    const bytes = await readCapped(resp, MAX_BODY_BYTES);
    if (bytes.byteLength === 0) {
      return { status, json: null };
    }
    try {
      return { status, json: JSON.parse(new TextDecoder().decode(bytes)) };
    } catch {
      throw new Error('upstream returned invalid JSON');
    }
  } finally {
    clearTimeout(timer);
  }
}

async function readCapped(resp: Response, max: number): Promise<Uint8Array> {
  if (!resp.body) return new Uint8Array(0);
  const reader = resp.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    total += value.byteLength;
    if (total > max) {
      await reader.cancel();
      throw new Error('upstream response exceeds 2 MiB');
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' },
  });
}

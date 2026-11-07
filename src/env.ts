import type { EventJob } from './qq';

/**
 * Worker 绑定。`QQ_*` 这些既可以是 wrangler secret，也可以是 `[vars]`，
 * 运行时会和 D1 `settings` 表里的值按优先级合并（见 config.ts）。
 */
export interface Env {
  DB: D1Database;
  KV: KVNamespace;
  EVENTS: Queue<EventJob>;
  QQ_APP_ID?: string;
  QQ_APP_SECRET?: string;
  QQ_API_BASE?: string;
  SKIP_SIGNATURE?: string;
  readonly [key: string]: unknown;
}

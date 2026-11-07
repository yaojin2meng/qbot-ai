import * as config from './config';
import type { Env } from './env';
import * as qq from './qq';
import type { EventJob } from './qq';
// 管理台页面直接读文件内嵌进 Worker（wrangler.toml 里配了 Text 规则）
import PAGE from '../web/index.html';

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' },
  });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(req.url);

    switch (pathname) {
      case '/qq/webhook':
        if (req.method !== 'POST') return new Response('method not allowed', { status: 405 });
        return qq.handleWebhook(req, env);

      case '/api/setup':
        if (req.method !== 'POST') return new Response('method not allowed', { status: 405 });
        return config.handleSetup(req, env);

      case '/api/status':
        return json(await config.status(env));

      case '/api/verify':
        if (req.method !== 'POST') return new Response('method not allowed', { status: 405 });
        return config.handleVerify(req, env);

      case '/':
        return new Response(PAGE, {
          headers: { 'Content-Type': 'text/html; charset=utf-8' },
        });

      default:
        return new Response('not found', { status: 404 });
    }
  },

  async queue(batch: MessageBatch<EventJob>, env: Env): Promise<void> {
    for (const message of batch.messages) {
      try {
        await qq.processJob(message.body, env);
        message.ack();
      } catch (e) {
        // 处理失败就重试；重试次数由 wrangler.toml 的 max_retries 控制。
        console.error('queue job failed:', e);
        message.retry();
      }
    }
  },
} satisfies ExportedHandler<Env, EventJob>;
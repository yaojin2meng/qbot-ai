mod config;
mod http;
mod model_test;
mod poll;
mod qq;
mod qq_api;
mod render;
mod security;

use worker::*;

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let router = Router::new();

    let result = router
        // ---- QQ 开放平台回调：群消息、按钮点击都从这里进来 ----
        .post_async("/qq/webhook", |req, ctx| async move {
            qq::handle_webhook(req, ctx).await
        })
        // ---- 首次初始化（只能用一次，之后自动锁死）----
        .post_async("/api/setup", |req, ctx| async move {
            config::handle_setup(req, ctx).await
        })
        // ---- 配置台状态自检 ----
        .get_async("/api/status", |req, ctx| async move {
            config::handle_status(req, ctx).await
        })
        // ---- 一键验证 QQ 凭据 ----
        .post_async("/api/verify", |req, ctx| async move {
            config::handle_verify(req, ctx).await
        })
        // ---- 配置台页面 ----
        .get_async("/", |_req, _ctx| async move {
            Response::from_html(include_str!("../web/index.html"))
        })
        .run(req, env)
        .await;

    let response = match result {
        Ok(response) => response,
        Err(e) => {
            console_error!("request failed: {e}");
            Response::error("Internal error; check worker logs", 500)?
        }
    };
    Ok(with_headers(response))
}

fn with_headers(mut response: Response) -> Response {
    let _ = response.headers_mut().set("Cache-Control", "no-store");
    let _ = response
        .headers_mut()
        .set("X-Content-Type-Options", "nosniff");
    let _ = response.headers_mut().set("Referrer-Policy", "no-referrer");
    let _ = response.headers_mut().set("X-Frame-Options", "DENY");
    response
}

#[event(queue)]
async fn queue(batch: MessageBatch<qq::EventJob>, env: Env, _ctx: Context) -> Result<()> {
    for message in batch.messages()? {
        match qq::process_job(message.body(), &env).await {
            Ok(()) => message.ack(),
            Err(e) => {
                console_error!("queued QQ task failed: {e} (message {})", message.id());
                message.retry();
            }
        }
    }
    Ok(())
}
mod admin;
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
    if req.path().starts_with("/api/") {
        if let Some(response) = security::authorize(&req, &env)? {
            return Ok(response);
        }
    }
    let router = Router::new();

    let result = router
        // QQ 开放平台回调（群消息、按钮点击等事件入口）
        .post_async("/qq/webhook", |req, ctx| async move {
            qq::handle_webhook(req, ctx).await
        })
        // Web UI / 管理 API
        .get_async("/api/configs", |req, ctx| async move {
            admin::list_configs(req, ctx).await
        })
        .post_async("/api/configs", |req, ctx| async move {
            admin::create_config(req, ctx).await
        })
        .delete_async("/api/configs/:id", |req, ctx| async move {
            admin::delete_config(req, ctx).await
        })
        .get_async("/api/configs/:id/models", |req, ctx| async move {
            model_test::list_models(req, ctx).await
        })
        .post_async("/api/configs/:id/test", |req, ctx| async move {
            model_test::run_test(req, ctx).await
        })
        .get_async("/api/polls", |req, ctx| async move {
            admin::list_polls(req, ctx).await
        })
        // 静态 Web UI（构建产物内嵌或单独托管）
        .get_async("/", |_req, _ctx| async move {
            Response::from_html(include_str!("../web/index.html"))
        })
        .run(req, env)
        .await;
    let mut response = match result {
        Ok(response) => response,
        Err(_) => {
            console_error!("request failed; check bindings, migrations and upstream permissions");
            Response::error("Internal error; check worker logs", 500)?
        }
    };
    response.headers_mut().set("Cache-Control", "no-store")?;
    response
        .headers_mut()
        .set("X-Content-Type-Options", "nosniff")?;
    response
        .headers_mut()
        .set("Referrer-Policy", "no-referrer")?;
    response.headers_mut().set("X-Frame-Options", "DENY")?;
    Ok(response)
}

#[event(queue)]
async fn queue(batch: MessageBatch<qq::EventJob>, env: Env, _ctx: Context) -> Result<()> {
    for message in batch.messages()? {
        match qq::process_job(message.body(), &env).await {
            Ok(()) => message.ack(),
            Err(_) => {
                console_error!(
                    "queued QQ task failed; retry scheduled (message {})",
                    message.id()
                );
                message.retry();
            }
        }
    }
    Ok(())
}

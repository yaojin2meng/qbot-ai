mod qq;
mod poll;
mod model_test;
mod render;
mod admin;

use worker::*;

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let router = Router::new();

    router
        // QQ 开放平台回调（群消息、按钮点击等事件入口）
        .post_async("/qq/webhook", |req, ctx| async move { qq::handle_webhook(req, ctx).await })
        // Web UI / 管理 API
        .get_async("/api/configs", |req, ctx| async move { admin::list_configs(req, ctx).await })
        .post_async("/api/configs", |req, ctx| async move { admin::create_config(req, ctx).await })
        .delete_async("/api/configs/:id", |req, ctx| async move { admin::delete_config(req, ctx).await })
        .get_async("/api/configs/:id/models", |req, ctx| async move { model_test::list_models(req, ctx).await })
        .post_async("/api/configs/:id/test", |req, ctx| async move { model_test::run_test(req, ctx).await })
        .get_async("/api/polls", |req, ctx| async move { admin::list_polls(req, ctx).await })
        // 静态 Web UI（构建产物内嵌或单独托管）
        .get_async("/", |_req, _ctx| async move {
            Response::from_html(include_str!("../web/index.html"))
        })
        .run(req, env)
        .await
}
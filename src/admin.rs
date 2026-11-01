use serde::Deserialize;
use serde_json::json;
use worker::*;

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

#[derive(Deserialize)]
struct CreateConfigBody {
    name: String,
    base_url: String,
    api_key: String,
}

/// GET /api/configs —— 列出所有 API 配置（api_key 打码）
pub async fn list_configs(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let db = ctx.env.d1("DB")?;
    let stmt = db.prepare(
        "SELECT id, name, base_url, api_key, created_at FROM api_configs ORDER BY id DESC",
    );
    let res = stmt.all().await?;
    let rows: Vec<serde_json::Value> = res.results::<serde_json::Value>()?;

    // api_key 打码
    let masked: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|mut r| {
            if let Some(k) = r.get("api_key").and_then(|v| v.as_str()) {
                let m = mask_key(k);
                r["api_key"] = json!(m);
            }
            r
        })
        .collect();

    Response::from_json(&json!({ "ok": true, "configs": masked }))
}

/// POST /api/configs —— 新增配置
pub async fn create_config(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let body: CreateConfigBody = match req.json().await {
        Ok(body) => body,
        Err(_) => return Response::error("invalid JSON", 400),
    };
    let base_url = match crate::model_test::normalize_base_url(&body.base_url) {
        Ok(url) => url,
        Err(_) => return Response::error("请输入不含凭据的公网 HTTPS API 地址", 400),
    };
    if body.name.len() > 200
        || body.api_key.len() > 4096
        || body.api_key.chars().any(char::is_control)
    {
        return Response::error("invalid name or key", 400);
    }
    if body.base_url.trim().is_empty() || body.api_key.trim().is_empty() {
        return Response::error("base_url / api_key 不能为空", 400);
    }
    let db = ctx.env.d1("DB")?;
    let res = db
        .prepare(
            "INSERT INTO api_configs (name, base_url, api_key, created_at) VALUES (?1,?2,?3,?4)",
        )
        .bind(&[
            body.name.into(),
            base_url.into(),
            body.api_key.trim().into(),
            now_ms().into(),
        ])?
        .run()
        .await?;
    let id = res.meta()?.and_then(|m| m.last_row_id).unwrap_or(0);
    Response::from_json(&json!({ "ok": true, "id": id }))
}

/// DELETE /api/configs/:id
pub async fn delete_config(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id = match ctx
        .param("id")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|id| *id > 0)
    {
        Some(id) => id,
        None => return Response::error("invalid id", 400),
    };
    let db = ctx.env.d1("DB")?;
    db.batch(vec![
        db.prepare("DELETE FROM model_tests WHERE config_id=?1")
            .bind(&[id.into()])?,
        db.prepare("DELETE FROM api_configs WHERE id=?1")
            .bind(&[id.into()])?,
    ])
    .await?;
    Response::from_json(&json!({ "ok": true }))
}

/// GET /api/polls —— 列出投票记录
pub async fn list_polls(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let db = ctx.env.d1("DB")?;
    let res = db
        .prepare(
            "SELECT poll_id, group_id, topic, target_count, agree, disagree, abstain, status, created_at, closed_at
             FROM polls ORDER BY created_at DESC LIMIT 100",
        )
        .all()
        .await?;
    let rows: Vec<serde_json::Value> = res.results::<serde_json::Value>()?;
    Response::from_json(&json!({ "ok": true, "polls": rows }))
}

fn mask_key(k: &str) -> String {
    let n = k.chars().count();
    if n <= 8 {
        return "****".into();
    }
    let head: String = k.chars().take(4).collect();
    let tail: String = k.chars().skip(n - 4).collect();
    format!("{}****{}", head, tail)
}

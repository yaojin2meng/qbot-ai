//! 运行时配置：一处读取、一处写入。
//!
//! 查找顺序：D1 `settings` 表 → `wrangler secret` → `wrangler.toml [vars]` → 内置默认值。
//! 网页配置台只在首次初始化时写入 AppID / AppSecret，之后只读。

use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::*;

pub const KEY_APP_ID: &str = "QQ_APP_ID";
pub const KEY_APP_SECRET: &str = "QQ_APP_SECRET";
pub const KEY_API_BASE: &str = "QQ_API_BASE";
pub const KEY_SKIP_SIGNATURE: &str = "SKIP_SIGNATURE";

pub const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";
const OFFICIAL_ORIGINS: [&str; 2] = ["https://api.sgroup.qq.com", "https://api.bot.qq.com"];

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    D1,
    Secret,
    Var,
    Missing,
}

/// 读取一项配置，同时告诉调用方它是从哪来的（用于配置台展示）。
pub async fn lookup(env: &Env, key: &str) -> (Option<String>, Source) {
    if let Ok(db) = env.d1("DB") {
        if let Ok(stmt) = db
            .prepare("SELECT value FROM settings WHERE key=?1")
            .bind(&[key.into()])
        {
            if let Ok(Some(v)) = stmt.first::<String>(Some("value")).await {
                if !v.trim().is_empty() {
                    return (Some(v), Source::D1);
                }
            }
        }
    }
    if let Ok(v) = env.secret(key) {
        let v = v.to_string();
        if !v.trim().is_empty() {
            return (Some(v), Source::Secret);
        }
    }
    if let Ok(v) = env.var(key) {
        let v = v.to_string();
        if !v.trim().is_empty() {
            return (Some(v), Source::Var);
        }
    }
    (None, Source::Missing)
}

pub async fn get(env: &Env, key: &str) -> Option<String> {
    lookup(env, key).await.0
}

pub async fn get_or(env: &Env, key: &str, fallback: &str) -> String {
    get(env, key).await.unwrap_or_else(|| fallback.to_string())
}

pub async fn flag(env: &Env, key: &str) -> bool {
    get(env, key)
        .await
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub async fn set(env: &Env, key: &str, value: &str) -> Result<()> {
    let db = env.d1("DB")?;
    let value = value.trim();
    if value.is_empty() {
        db.prepare("DELETE FROM settings WHERE key=?1")
            .bind(&[key.into()])?
            .run()
            .await?;
    } else {
        db.prepare(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        )
        .bind(&[key.into(), value.into(), now_ms().into()])?
        .run()
        .await?;
    }
    Ok(())
}

async fn put(env: &Env, key: &str, value: &str) -> std::result::Result<(), String> {
    set(env, key, value)
        .await
        .map_err(|e| format!("写入 D1 失败（迁移 0003_settings.sql 是否已执行？）：{e}"))
}

// ---------------------------------------------------------------------------
// 状态自检
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub configured: bool,
    pub source: Source,
    pub required: bool,
    pub secret: bool,
    /// 非敏感项的当前值，用于表单回填；敏感项永远为 null。
    pub value: Option<String>,
    pub hint: &'static str,
}

#[derive(Serialize)]
pub struct Bindings {
    pub db: bool,
    pub kv: bool,
    pub queue: bool,
}

#[derive(Serialize)]
pub struct Status {
    /// AppID 与 AppSecret 都填好才算初始化完成。
    pub setup_required: bool,
    pub fields: Vec<Field>,
    pub bindings: Bindings,
    pub webhook_path: &'static str,
    pub events: [&'static str; 2],
    pub permissions: [&'static str; 4],
    pub commands: [&'static str; 3],
}

pub async fn status(env: &Env) -> Status {
    let specs: [(&'static str, &'static str, bool, bool, &'static str); 4] = [
        (
            KEY_APP_ID,
            "QQ_APP_ID",
            true,
            true,
            "QQ 开放平台 → 机器人 → 开发设置 → AppID",
        ),
        (
            KEY_APP_SECRET,
            "QQ_APP_SECRET",
            true,
            true,
            "同页面的 AppSecret。取 AccessToken 和回调验签都用它",
        ),
        (
            KEY_API_BASE,
            "QQ_API_BASE",
            false,
            false,
            "默认 api.sgroup.qq.com；遇到 403 换成 api.bot.qq.com",
        ),
        (
            KEY_SKIP_SIGNATURE,
            "SKIP_SIGNATURE",
            false,
            false,
            "只在本地调试时开；线上必须 false",
        ),
    ];

    let mut fields = Vec::with_capacity(specs.len());
    for (key, label, required, secret, hint) in specs {
        let (value, source) = lookup(env, key).await;
        fields.push(Field {
            key,
            label,
            required,
            secret,
            configured: value.is_some(),
            source,
            value: if secret { None } else { value },
            hint,
        });
    }

    let setup_required = !fields
        .iter()
        .filter(|f| f.required)
        .all(|f| f.configured);

    Status {
        setup_required,
        fields,
        bindings: Bindings {
            db: env.d1("DB").is_ok(),
            kv: env.kv("KV").is_ok(),
            queue: env.queue("EVENTS").is_ok(),
        },
        webhook_path: "/qq/webhook",
        events: ["GROUP_AT_MESSAGE_CREATE", "INTERACTION_CREATE"],
        permissions: [
            "群聊能力（群场景必需）",
            "主动消息",
            "Markdown 消息",
            "消息按钮（自定义按钮为内邀能力）",
        ],
        commands: [
            "投票 <主题> <人数>",
            "测速 <Base URL> <API Key>",
            "帮助",
        ],
    }
}

pub async fn handle_status(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    Response::from_json(&status(&ctx.env).await)
}

// ---------------------------------------------------------------------------
// 首次初始化
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SetupBody {
    app_id: String,
    app_secret: String,
    #[serde(default)]
    api_base: Option<String>,
}

pub async fn handle_setup(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    // 已经初始化过就不允许再写，避免被匿名请求覆盖配置。
    let (app_id_now, _) = lookup(&ctx.env, KEY_APP_ID).await;
    let (secret_now, _) = lookup(&ctx.env, KEY_APP_SECRET).await;
    if app_id_now.is_some() && secret_now.is_some() {
        return Response::from_json(&json!({
            "ok": false,
            "message": "已经初始化过了，无需重复配置。"
        }));
    }
    let body: SetupBody = match req.json().await {
        Ok(b) => b,
        Err(_) => {
            return Response::from_json(&json!({"ok": false, "message": "请求格式错误"}))
        }
    };
    let app_id = body.app_id.trim();
    let app_secret = body.app_secret.trim();

    if app_id.is_empty() || app_secret.is_empty() {
        return Response::from_json(&json!({
            "ok": false,
            "message": "QQ_APP_ID / QQ_APP_SECRET 都不能为空"
        }));
    }
    if let Some(base) = body.api_base.as_deref() {
        if let Err(m) = check_origin(base) {
            return Response::from_json(&json!({"ok": false, "message": m}));
        }
    }

    for (key, value) in [
        (KEY_APP_ID, app_id),
        (KEY_APP_SECRET, app_secret),
    ] {
        if let Err(m) = put(&ctx.env, key, value).await {
            return Response::from_json(&json!({"ok": false, "message": m}));
        }
    }
    if let Some(base) = body.api_base.as_deref() {
        if let Err(m) = put(&ctx.env, KEY_API_BASE, base).await {
            return Response::from_json(&json!({"ok": false, "message": m}));
        }
    }

    Response::from_json(&json!({"ok": true, "message": "初始化完成"}))
}

fn check_origin(base: &str) -> std::result::Result<(), String> {
    let base = base.trim();
    if base.is_empty() {
        return Ok(());
    }
    let base = base.trim_end_matches('/');
    if OFFICIAL_ORIGINS.contains(&base) {
        Ok(())
    } else {
        Err(format!(
            "QQ_API_BASE 只能是 {} 之一",
            OFFICIAL_ORIGINS.join(" 或 ")
        ))
    }
}

// ---------------------------------------------------------------------------
// 一键验证凭据
// ---------------------------------------------------------------------------

pub async fn handle_verify(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    match crate::qq_api::probe(&ctx.env).await {
        Ok(()) => Response::from_json(&json!({
            "ok": true,
            "message": "凭据可用，已成功拿到 QQ AccessToken"
        })),
        Err(e) => Response::from_json(&json!({
            "ok": false,
            "message": format!("拿不到 AccessToken：{e}")
        })),
    }
}
use worker::*;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct ConfigRow {
    id: i64,
    base_url: String,
    api_key: String,
}

#[derive(Deserialize)]
struct TestBody {
    models: Option<Vec<String>>,
}

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

/// 识别文本中的 API 与 Key：
/// 1. 识别 JSON: {"_type":"newapi_channel_conn","key":"sk-xxx","url":"https://ai.apizn.com"}
/// 2. 识别两行/混合文本：url 和 key
pub fn extract_api_and_key(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();

    // 格式一：JSON 格式 (支持周围有文字时截取 {...})
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            let json_str = &trimmed[start..=end];
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                let url = val.get("url").or_else(|| val.get("base_url")).and_then(|v| v.as_str());
                let key = val.get("key").or_else(|| val.get("api_key")).and_then(|v| v.as_str());
                if let (Some(u), Some(k)) = (url, key) {
                    if !u.trim().is_empty() && !k.trim().is_empty() {
                        return Some((u.trim().trim_end_matches('/').to_string(), k.trim().to_string()));
                    }
                }
            }
        }
    }

    // 格式二：按行提取 url 和 key（支持带有 url: / key: 标签）
    let mut detected_url: Option<String> = None;
    let mut detected_key: Option<String> = None;

    for line in trimmed.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        let lower = l.to_lowercase();
        let cleaned = if lower.starts_with("url:") || lower.starts_with("url=") {
            l[4..].trim()
        } else if lower.starts_with("key:") || lower.starts_with("key=") {
            l[4..].trim()
        } else if lower.starts_with("api_key:") || lower.starts_with("api_key=") {
            l[8..].trim()
        } else if lower.starts_with("base_url:") || lower.starts_with("base_url=") {
            l[9..].trim()
        } else {
            l
        };

        if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
            if detected_url.is_none() {
                detected_url = Some(cleaned.trim_end_matches('/').to_string());
            }
        } else if cleaned.starts_with("sk-") || (!cleaned.contains("://") && cleaned.len() >= 16) {
            if detected_key.is_none() {
                detected_key = Some(cleaned.to_string());
            }
        }
    }

    if let (Some(u), Some(k)) = (detected_url, detected_key) {
        return Some((u, k));
    }

    // 格式三：同一行由空格分隔 "https://xxx sk-xxx"
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 2 {
        if parts[0].starts_with("http://") || parts[0].starts_with("https://") {
            return Some((parts[0].trim_end_matches('/').to_string(), parts[1].to_string()));
        } else if parts[1].starts_with("http://") || parts[1].starts_with("https://") {
            return Some((parts[1].trim_end_matches('/').to_string(), parts[0].to_string()));
        }
    }

    None
}

/// 群内收到 API 配置时自动执行测速并回复群聊（防空回、生成测试图）
pub async fn test_and_reply_group(
    ctx: &RouteContext<()>,
    group_id: &str,
    msg_id: &str,
    base_url: &str,
    api_key: &str,
) -> Result<()> {
    // 1. 先回复一条正在测试的消息提示
    let hint_payload = json!({
        "msg_type": 2,
        "markdown": {
            "content": format!("🔍 **已识别 API 配置**\n\n> URL: `{}`\n\n⏳ 正在拉取可用模型并进行防空回随机测速，请稍候...", base_url)
        },
        "msg_id": msg_id,
        "msg_seq": 1
    });
    let _ = send_group_payload(ctx, group_id, hint_payload).await;

    // 2. 拉取所有模型
    let models = match fetch_model_list(base_url, api_key).await {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            let err_payload = json!({
                "msg_type": 2,
                "markdown": { "content": format!("⚠️ 未在该接口检索到可用模型列表（/v1/models 返回空）。") },
                "msg_id": msg_id,
                "msg_seq": 2
            });
            let _ = send_group_payload(ctx, group_id, err_payload).await;
            return Ok(());
        }
        Err(e) => {
            let err_payload = json!({
                "msg_type": 2,
                "markdown": { "content": format!("❌ 拉取模型列表失败: {}", e) },
                "msg_id": msg_id,
                "msg_seq": 2
            });
            let _ = send_group_payload(ctx, group_id, err_payload).await;
            return Ok(());
        }
    };

    // 3. 并发测试模型（最多取 30 个）
    let mut results: Vec<ModelResult> = Vec::new();
    let mut handles = Vec::new();
    for model in models.into_iter().take(30) {
        let base = base_url.to_string();
        let key = api_key.to_string();
        let m = model.clone();
        handles.push(async move { test_one(0, &base, &key, &m).await });
    }
    for h in handles {
        results.push(h.await);
    }

    // 4. 生成测速结果报告图
    let png_bytes = crate::render::render_results_png_bytes(&results);

    // 5. 优先通过 QQ 群富媒体发送图片；若未开通图片权限则发送精美 Markdown 汇总
    let sent_image = if let Some(bytes) = png_bytes {
        upload_and_send_group_image(ctx, group_id, msg_id, bytes).await.is_ok()
    } else {
        false
    };

    if !sent_image {
        // 降级为 Markdown 表格发送
        let mut md = format!("# 📊 模型测速报告\n\n> URL: `{}`\n\n| 模型 | 状态 | 耗时/错误 |\n|---|---|---|\n", base_url);
        for r in &results {
            let status = if r.ok {
                "✅ 成功"
            } else if r.error.as_deref().map(|e| e.contains("空回")).unwrap_or(false) {
                "⚠️ 空回"
            } else if r.error.as_deref().map(|e| e.contains("超时")).unwrap_or(false) {
                "⏱️ 超时"
            } else {
                "❌ 错误"
            };
            let detail = if r.ok {
                format!("{} ms", r.latency_ms.unwrap_or(0))
            } else {
                r.error.clone().unwrap_or_default()
            };
            md.push_str(&format!("| {} | {} | {} |\n", r.model, status, detail));
        }
        let reply_payload = json!({
            "msg_type": 2,
            "markdown": { "content": md },
            "msg_id": msg_id,
            "msg_seq": 3
        });
        let _ = send_group_payload(ctx, group_id, reply_payload).await;
    }

    Ok(())
}

/// 上传富媒体文件并发送群图片
async fn upload_and_send_group_image(
    ctx: &RouteContext<()>,
    group_id: &str,
    msg_id: &str,
    png_bytes: Vec<u8>,
) -> Result<()> {
    let token = crate::poll::get_access_token(ctx).await?;
    let upload_url = format!("https://api.sgroup.qq.com/v2/groups/{}/files", group_id);

    use base64::{engine::general_purpose::STANDARD, Engine};
    let b64_file = STANDARD.encode(&png_bytes);

    let upload_body = json!({
        "file_type": 1, // 1: 图片
        "file_data": b64_file,
        "srv_send_msg": false
    });

    let headers = Headers::new();
    headers.set("Authorization", &format!("QQBot {}", token))?;
    headers.set("Content-Type", "application/json")?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(upload_body.to_string().into()));

    let req = Request::new_with_init(&upload_url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let resp_json: serde_json::Value = resp.json().await?;

    let file_info = resp_json
        .get("file_info")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::from("file_info not returned from upload"))?;

    // 发送富媒体消息 (msg_type=7)
    let msg_payload = json!({
        "msg_type": 7,
        "media": {
            "file_info": file_info
        },
        "msg_id": msg_id,
        "msg_seq": 2
    });
    send_group_payload(ctx, group_id, msg_payload).await?;
    Ok(())
}

async fn send_group_payload(
    ctx: &RouteContext<()>,
    group_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let token = crate::poll::get_access_token(ctx).await?;
    let url = format!("https://api.sgroup.qq.com/v2/groups/{}/messages", group_id);

    let headers = Headers::new();
    headers.set("Authorization", &format!("QQBot {}", token))?;
    headers.set("Content-Type", "application/json")?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(payload.to_string().into()));

    let req = Request::new_with_init(&url, &init)?;
    let _ = Fetch::Request(req).send().await?;
    Ok(())
}

/// GET /api/configs/:id/models —— Web UI 端拉取模型
pub async fn list_models(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id: i64 = ctx.param("id").unwrap().parse().unwrap_or(0);
    let db = ctx.env.d1("DB")?;
    let cfg = db
        .prepare("SELECT id, base_url, api_key FROM api_configs WHERE id = ?1")
        .bind(&[id.into()])?
        .first::<ConfigRow>(None)
        .await?;

    let cfg = match cfg {
        Some(c) => c,
        None => return Response::error("config not found", 404),
    };

    let models = fetch_model_list(&cfg.base_url, &cfg.api_key).await?;
    Response::from_json(&json!({ "ok": true, "models": models }))
}

/// POST /api/configs/:id/test —— Web UI 端并发测速
pub async fn run_test(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id: i64 = ctx.param("id").unwrap().parse().unwrap_or(0);
    let db = ctx.env.d1("DB")?;
    let cfg = db
        .prepare("SELECT id, base_url, api_key FROM api_configs WHERE id = ?1")
        .bind(&[id.into()])?
        .first::<ConfigRow>(None)
        .await?;
    let cfg = match cfg {
        Some(c) => c,
        None => return Response::error("config not found", 404),
    };

    let body: TestBody = req.json().await.unwrap_or(TestBody { models: None });
    let models = match body.models {
        Some(m) if !m.is_empty() => m,
        _ => fetch_model_list(&cfg.base_url, &cfg.api_key).await?,
    };

    let mut results: Vec<ModelResult> = Vec::new();
    let mut handles = Vec::new();

    for model in models.into_iter().take(30) {
        let base = cfg.base_url.clone();
        let key = cfg.api_key.clone();
        let cfg_id = cfg.id;
        let m = model.clone();
        handles.push(async move { test_one(cfg_id, &base, &key, &m).await });
    }

    for h in handles {
        let r = h.await;
        let _ = db
            .prepare(
                "INSERT INTO model_tests (config_id, model, ok, latency_ms, error, prompt, answer, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )
            .bind(&[
                r.config_id.into(),
                r.model.clone().into(),
                (r.ok as i64).into(),
                r.latency_ms.into(),
                r.error.clone().unwrap_or_default().into(),
                r.prompt.clone().into(),
                r.answer.clone().into(),
                now_ms().into(),
            ])?
            .run()
            .await;
        results.push(r);
    }

    let png_data_url = crate::render::render_results_png(&results);

    Response::from_json(&json!({
        "ok": true,
        "results": results,
        "image": png_data_url,
    }))
}

async fn fetch_model_list(base_url: &str, api_key: &str) -> Result<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {}", api_key))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let req = Request::new_with_init(&url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|x| x.as_str()).map(String::from)).collect())
        .unwrap_or_default())
}

#[derive(serde::Serialize, Clone)]
pub struct ModelResult {
    #[serde(skip)]
    pub config_id: i64,
    pub model: String,
    pub ok: bool,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub prompt: String,
    pub answer: String,
}

/// 随机产生非 "hi" 试题，防空回 & 避免命中缓存
fn random_prompt() -> String {
    let kind: u32 = rand::random::<u32>() % 3;
    match kind {
        0 => {
            let a: i64 = (rand::random::<u32>() % 900 + 100) as i64;
            let b: i64 = (rand::random::<u32>() % 90 + 10) as i64;
            let c: i64 = (rand::random::<u32>() % 500 + 10) as i64;
            format!("请只输出计算结果，不要解释：{} * {} + {} =", a, b, c)
        }
        1 => {
            let chars = "abcdefghijklmnopqrstuvwxyz0123456789";
            let s: String = (0..8)
                .map(|_| {
                    let i = rand::random::<u32>() as usize % chars.len();
                    chars.chars().nth(i).unwrap()
                })
                .collect();
            format!("将字符串 '{}' 完全逆序输出，只输出逆序结果：", s)
        }
        _ => {
            let nums: Vec<i64> = (0..5).map(|_| (rand::random::<u32>() % 100) as i64).collect();
            format!(
                "请只输出以下数字之和的阿拉伯数字结果：{}",
                nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" + ")
            )
        }
    }
}

/// 实际请求测试单个模型（带耗时记录与防空回判断）
async fn test_one(config_id: i64, base_url: &str, api_key: &str, model: &str) -> ModelResult {
    let prompt = random_prompt();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.2,
        "max_tokens": 64,
        "stream": false
    });

    let headers = Headers::new();
    let _ = headers.set("Authorization", &format!("Bearer {}", api_key));
    let _ = headers.set("Content-Type", "application/json");

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(payload.to_string().into()));

    let start = now_ms();
    let req = match Request::new_with_init(&url, &init) {
        Ok(r) => r,
        Err(e) => return fail(config_id, model, &prompt, format!("构造请求失败: {e}")),
    };

    let resp = match Fetch::Request(req).send().await {
        Ok(r) => r,
        Err(e) => return fail(config_id, model, &prompt, format!("请求失败/超时: {e}")),
    };
    let status = resp.status_code();
    let mut resp = resp;
    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
    let elapsed = now_ms() - start;

    if status != 200 {
        return fail(config_id, model, &prompt, format!("HTTP {}: {}", status, v));
    }

    let content = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // 防空回验证：返回内容为空判定为测试失败
    if content.is_empty() {
        return fail(config_id, model, &prompt, "空回（返回内容为空）".into());
    }

    ModelResult {
        config_id,
        model: model.to_string(),
        ok: true,
        latency_ms: Some(elapsed),
        error: None,
        prompt,
        answer: truncate(&content, 120),
    }
}

fn fail(config_id: i64, model: &str, prompt: &str, err: String) -> ModelResult {
    ModelResult {
        config_id,
        model: model.to_string(),
        ok: false,
        latency_ms: None,
        error: Some(err),
        prompt: prompt.to_string(),
        answer: String::new(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
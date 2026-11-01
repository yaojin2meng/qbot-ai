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
    models: Option<Vec<String>>, // 不传则测全部
}

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

/// GET /api/configs/:id/models —— 拉取该配置可用的模型列表
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

    let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {}", cfg.api_key))?;

    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let req = Request::new_with_init(&url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let status = resp.status_code();
    let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));

    if status != 200 {
        return Response::from_json(&json!({ "ok": false, "status": status, "raw": v }));
    }

    let models: Vec<String> = v
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Response::from_json(&json!({ "ok": true, "models": models }))
}

/// POST /api/configs/:id/test —— 并发测试模型，防空回，记录结果，返回汇总(含生成的图片)
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
        _ => {
            // 未指定则自动拉全部
            let r = list_models_internal(&cfg).await?;
            r
        }
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
        // 落库
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

    // 生成结果图片（SVG -> PNG），返回 base64 data URL
    let png_data_url = crate::render::render_results_png(&results);

    Response::from_json(&json!({
        "ok": true,
        "results": results,
        "image": png_data_url,
    }))
}

async fn list_models_internal(cfg: &ConfigRow) -> Result<Vec<String>> {
    let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {}", cfg.api_key))?;
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

/// 生成随机测试题（避免 hi 这类无意义题，防空回/缓存）
fn random_prompt() -> String {
    let kind: u32 = rand::random::<u32>() % 3;
    match kind {
        0 => {
            // 随机四则运算
            let a: i64 = (rand::random::<u32>() % 900 + 100) as i64;
            let b: i64 = (rand::random::<u32>() % 90 + 10) as i64;
            let c: i64 = (rand::random::<u32>() % 500 + 10) as i64;
            format!("请只输出计算结果，不要解释：{} * {} + {} =", a, b, c)
        }
        1 => {
            // 随机字符串逆序
            let chars = "abcdefghijklmnopqrstuvwxyz0123456789";
            let s: String = (0..8).map(|_| {
                let i = rand::random::<u32>() as usize % chars.len();
                chars.chars().nth(i).unwrap()
            }).collect();
            format!("将字符串 '{}' 完全逆序输出，只输出逆序结果：", s)
        }
        _ => {
            // 随机数字求和
            let nums: Vec<i64> = (0..5).map(|_| (rand::random::<u32>() % 100) as i64).collect();
            format!("请只输出以下数字之和的阿拉伯数字结果：{}", nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" + "))
        }
    }
}

/// 测试单个模型（带超时的 fetch，防空回）
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

    // 防空回：内容为空则视为失败
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
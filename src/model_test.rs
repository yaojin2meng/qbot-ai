use futures_util::{stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use worker::*;

const MAX_MODELS: usize = 30;
const CONCURRENCY: usize = 5;
const REQUEST_TIMEOUT: u64 = 15;

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

/// A bare origin uses /v1. Explicit versioned/custom API paths are preserved.
/// HTTPS only; prohibit literal IPs, userinfo, query/fragment and local-only names.
pub fn normalize_base_url(input: &str) -> Result<String> {
    if input.len() > 2048 {
        return Err(Error::from("API URL too long"));
    }
    let mut url = Url::parse(input.trim()).map_err(|_| Error::from("invalid API URL"))?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
        || host.parse::<std::net::IpAddr>().is_ok()
        || host.starts_with('[')
        || !host.contains('.')
        || host.ends_with('.')
        || [".localhost", ".local", ".internal", ".test", ".invalid"]
            .iter()
            .any(|s| host.ends_with(s))
    {
        return Err(Error::from(
            "only public HTTPS API origins without credentials are allowed",
        ));
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/v1" } else { &path });
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn config_pair(url: &str, key: &str) -> Option<(String, String)> {
    let key = key.trim();
    if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
        return None;
    }
    Some((normalize_base_url(url).ok()?, key.to_string()))
}

/// JSON, two labelled lines, or a URL and key separated by whitespace.
pub fn extract_api_and_key(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            if let Ok(v) = serde_json::from_str::<Value>(&text[start..=end]) {
                let url = v
                    .get("url")
                    .or_else(|| v.get("base_url"))
                    .and_then(Value::as_str);
                let key = v
                    .get("key")
                    .or_else(|| v.get("api_key"))
                    .and_then(Value::as_str);
                if let (Some(url), Some(key)) = (url, key) {
                    return config_pair(url, key);
                }
            }
        }
    }
    let parts: Vec<_> = text.split_whitespace().collect();
    if parts.len() == 2 {
        if parts[0].starts_with("https://") {
            return config_pair(parts[0], parts[1]);
        }
        if parts[1].starts_with("https://") {
            return config_pair(parts[1], parts[0]);
        }
    }
    let mut url = None;
    let mut key = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let lower = line.to_ascii_lowercase();
        let mut value = line;
        let mut key_label = false;
        for label in [
            "url:",
            "url=",
            "base_url:",
            "base_url=",
            "key:",
            "key=",
            "api_key:",
            "api_key=",
        ] {
            if lower.starts_with(label) {
                value = line[label.len()..].trim();
                key_label = label.starts_with("key") || label.starts_with("api_key");
                break;
            }
        }
        if value.starts_with("https://") && url.is_none() {
            url = Some(value);
        } else if (key_label || value.starts_with("sk-"))
            && !value.contains(char::is_whitespace)
            && key.is_none()
        {
            key = Some(value);
        }
    }
    config_pair(url?, key?)
}

pub async fn test_and_reply_group(
    env: &Env,
    group: &str,
    msg: &str,
    base: &str,
    key: &str,
) -> Result<()> {
    let base = normalize_base_url(base)?;
    // The progress message is optional; failure is logged, not mistaken for successful delivery.
    if crate::qq_api::send_group(
        env,
        group,
        json!({
            "msg_type": 0, "content": "已识别 API，正在测速（最多 30 个模型，单个超时 15 秒）。",
            "msg_id": msg, "msg_seq": 1
        }),
    )
    .await
    .is_err()
    {
        console_error!("QQ test progress message failed");
    }
    let models = match fetch_model_list(&base, key).await {
        Ok(models) if !models.is_empty() => models,
        _ => {
            return crate::qq_api::send_group(env, group, json!({
                "msg_type": 0, "content": "拉取模型失败或列表为空，请检查 API Base URL、Key 与接口权限。",
                "msg_id": msg, "msg_seq": 2
            })).await;
        }
    };
    let results = test_models(&base, key, models).await;
    if let Some(bytes) = crate::render::render_results_png_bytes(&results) {
        match upload_and_send_group_image(env, group, msg, bytes).await {
            Ok(()) => return Ok(()),
            Err(_) => console_error!("QQ test image upload/send failed; using text fallback"),
        }
    }
    // Plain text needs fewer QQ permissions and avoids Markdown injection by upstream model names.
    let mut report = String::from("模型测速报告\n");
    for r in &results {
        let detail = if r.ok {
            format!("{} ms", r.latency_ms.unwrap_or(0))
        } else {
            r.error.clone().unwrap_or_else(|| "失败".into())
        };
        report.push_str(&format!("{}: {}\n", truncate(&r.model, 60), detail));
    }
    crate::qq_api::send_group(
        env,
        group,
        json!({
            "msg_type": 0, "content": report, "msg_id": msg, "msg_seq": 3
        }),
    )
    .await
}

async fn upload_and_send_group_image(
    env: &Env,
    group: &str,
    msg: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let value = crate::qq_api::request(
        env,
        Method::Post,
        &format!("/v2/groups/{group}/files"),
        json!({
            "file_type": 1, "file_data": STANDARD.encode(bytes), "srv_send_msg": false
        }),
    )
    .await?;
    let info = value
        .get("file_info")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::from("QQ file upload returned no file_info"))?;
    crate::qq_api::send_group(
        env,
        group,
        json!({
            "msg_type": 7, "media": {"file_info": info}, "msg_id": msg, "msg_seq": 2
        }),
    )
    .await
}

async fn fetch_model_list(base: &str, key: &str) -> Result<Vec<String>> {
    let base = normalize_base_url(base)?;
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {key}"))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error);
    let req = Request::new_with_init(&format!("{base}/models"), &init)?;
    let (status, value) = crate::http::fetch_json(req, REQUEST_TIMEOUT).await?;
    if status != 200 {
        return Err(Error::from(format!("model list HTTP {status}")));
    }
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::from("invalid model list"))?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .take(1000)
        .map(String::from)
        .collect();
    models.sort();
    models.dedup();
    Ok(models)
}

async fn test_models(base: &str, key: &str, models: Vec<String>) -> Vec<ModelResult> {
    // buffer_unordered polls up to CONCURRENCY futures at once; a Vec<Future> alone does not.
    let futures = models
        .into_iter()
        .take(MAX_MODELS)
        .enumerate()
        .map(|(index, model)| async move { (index, test_one(base, key, &model).await) });
    let mut results: Vec<_> = stream::iter(futures)
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

#[derive(Serialize, Clone)]
pub struct ModelResult {
    pub model: String,
    pub ok: bool,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub prompt: String,
    pub answer: String,
}

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
            let nums: Vec<i64> = (0..5)
                .map(|_| (rand::random::<u32>() % 100) as i64)
                .collect();
            format!(
                "请只输出以下数字之和的阿拉伯数字结果：{}",
                nums.iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(" + ")
            )
        }
    }
}

async fn test_one(base: &str, key: &str, model: &str) -> ModelResult {
    let prompt = random_prompt();
    let payload = json!({"model": model, "messages": [{"role":"user", "content":prompt}],
        "temperature":0.2, "max_tokens":64, "stream":false});
    let headers = Headers::new();
    if headers
        .set("Authorization", &format!("Bearer {key}"))
        .is_err()
        || headers.set("Content-Type", "application/json").is_err()
    {
        return fail(model, &prompt, "请求头无效".into());
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error)
        .with_body(Some(payload.to_string().into()));
    let req = match Request::new_with_init(&format!("{base}/chat/completions"), &init) {
        Ok(req) => req,
        Err(_) => return fail(model, &prompt, "构造请求失败".into()),
    };
    let start = now_ms();
    let (status, value) = match crate::http::fetch_json(req, REQUEST_TIMEOUT).await {
        Ok(result) => result,
        Err(_) => return fail(model, &prompt, "请求失败/超时/非 JSON 响应".into()),
    };
    if status != 200 {
        return fail(model, &prompt, format!("HTTP {status}"));
    }
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        return fail(model, &prompt, "空回（返回内容为空）".into());
    }
    ModelResult {
        model: model.into(),
        ok: true,
        latency_ms: Some(now_ms() - start),
        error: None,
        prompt,
        answer: truncate(content, 120).replace(key, "[REDACTED]"),
    }
}

fn fail(model: &str, prompt: &str, error: String) -> ModelResult {
    ModelResult {
        model: model.into(),
        ok: false,
        latency_ms: None,
        error: Some(error),
        prompt: prompt.into(),
        answer: String::new(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.into()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls() {
        assert_eq!(
            normalize_base_url("https://api.example.com/").unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.example.com/custom/v2/").unwrap(),
            "https://api.example.com/custom/v2"
        );
        for s in [
            "http://api.example.com",
            "https://127.0.0.1",
            "https://[::1]",
            "https://localhost",
            "https://x.local",
            "https://user:pass@api.example.com",
            "https://api.example.com?key=a",
        ] {
            assert!(normalize_base_url(s).is_err(), "{s}");
        }
    }
    #[test]
    fn extraction() {
        let expected = Some(("https://api.example.com/v1".into(), "sk-abc".into()));
        assert_eq!(
            extract_api_and_key("https://api.example.com sk-abc"),
            expected
        );
        assert_eq!(
            extract_api_and_key("URL: https://api.example.com\nKEY: sk-abc"),
            expected
        );
        assert_eq!(
            extract_api_and_key(r#"{"url":"https://api.example.com","key":"sk-abc"}"#),
            expected
        );
        assert_eq!(extract_api_and_key("只是普通群消息"), None);
    }
}

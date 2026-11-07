use serde_json::{json, Value};
use worker::*;

async fn api_origin(env: &Env) -> Result<String> {
    let origin = crate::config::get_or(env, crate::config::KEY_API_BASE, crate::config::DEFAULT_API_BASE)
        .await;
    let origin = origin.trim().trim_end_matches('/');
    if !["https://api.sgroup.qq.com", "https://api.bot.qq.com"].contains(&origin) {
        return Err(Error::from(
            "QQ_API_BASE 必须是 api.sgroup.qq.com 或 api.bot.qq.com",
        ));
    }
    Ok(origin.to_string())
}

pub async fn request(env: &Env, method: Method, path: &str, body: Value) -> Result<Value> {
    let token = get_access_token(env).await?;
    let headers = Headers::new();
    headers.set("Authorization", &format!("QQBot {token}"))?;
    headers.set("Content-Type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(method)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error)
        .with_body(Some(body.to_string().into()));
    let req = Request::new_with_init(&format!("{}{path}", api_origin(env).await?), &init)?;
    let (status, value) = crate::http::fetch_json(req, 10).await?;
    if status == 401 {
        if let Err(e) = invalidate_token(env).await {
            console_error!("could not invalidate QQ token cache: {e}");
        }
    }
    let code = value
        .get("code")
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0);
    if !(200..300).contains(&status) || code != 0 {
        // Never include upstream response bodies: they may echo credentials or user content.
        return Err(Error::from(format!(
            "QQ API rejected request (HTTP {status}, code {code})"
        )));
    }
    Ok(value)
}

pub async fn send_group(env: &Env, group_id: &str, payload: Value) -> Result<()> {
    if !valid_id(group_id) {
        return Err(Error::from("invalid group id"));
    }
    request(
        env,
        Method::Post,
        &format!("/v2/groups/{group_id}/messages"),
        payload,
    )
    .await?;
    Ok(())
}

pub async fn ack_interaction(env: &Env, id: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(Error::from("invalid interaction id"));
    }
    request(
        env,
        Method::Put,
        &format!("/interactions/{id}"),
        json!({"code": 0}),
    )
    .await?;
    Ok(())
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

async fn get_access_token(env: &Env) -> Result<String> {
    let app_id = crate::config::get(env, crate::config::KEY_APP_ID)
        .await
        .ok_or_else(|| Error::from("未配置 QQ_APP_ID，请打开网页配置台填写"))?;
    let secret = crate::config::get(env, crate::config::KEY_APP_SECRET)
        .await
        .ok_or_else(|| Error::from("未配置 QQ_APP_SECRET，请打开网页配置台填写"))?;
    if app_id.trim().is_empty() || secret.is_empty() {
        return Err(Error::from("QQ 凭据为空，请打开网页配置台填写"));
    }
    let kv = env.kv("KV")?;
    let cache_key = format!("qq_access_token:{app_id}");
    if let Ok(Some(token)) = kv.get(&cache_key).text().await {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error)
        .with_body(Some(
            json!({"appId": app_id, "clientSecret": secret})
                .to_string()
                .into(),
        ));
    let req = Request::new_with_init("https://bots.qq.com/app/getAppAccessToken", &init)?;
    let (status, value) = crate::http::fetch_json(req, 8).await?;
    if status != 200 {
        return Err(Error::from(format!(
            "QQ 取 AccessToken 失败（HTTP {status}），请检查 QQ_APP_ID / QQ_APP_SECRET"
        )));
    }
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::from("QQ 返回里没有 access_token，请检查 QQ_APP_ID / QQ_APP_SECRET"))?
        .to_string();
    let expires = value
        .get("expires_in")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .ok_or_else(|| Error::from("QQ token endpoint returned invalid expires_in"))?;
    // KV minimum TTL is 60s; do not cache tokens whose remaining lifetime is too short.
    if expires >= 120 {
        if kv
            .put(&cache_key, &token)?
            .expiration_ttl(expires - 60)
            .execute()
            .await
            .is_err()
        {
            console_error!("QQ token cache write failed");
        }
    }
    Ok(token)
}

async fn invalidate_token(env: &Env) -> Result<()> {
    let app_id = crate::config::get(env, crate::config::KEY_APP_ID)
        .await
        .unwrap_or_default();
    if app_id.is_empty() {
        return Ok(());
    }
    let kv = env.kv("KV")?;
    kv.delete(&format!("qq_access_token:{app_id}")).await?;
    Ok(())
}

/// 配置台用：走一遍真实的取 token 流程，验证 AppID / AppSecret 是否可用。
/// 每次都跳过缓存直连上游，避免用旧 token 给出假阳性。
pub async fn probe(env: &Env) -> Result<()> {
    let app_id = crate::config::get(env, crate::config::KEY_APP_ID)
        .await
        .ok_or_else(|| Error::from("还没有填写 QQ_APP_ID"))?;
    let secret = crate::config::get(env, crate::config::KEY_APP_SECRET)
        .await
        .ok_or_else(|| Error::from("还没有填写 QQ_APP_SECRET"))?;
    if app_id.trim().is_empty() || secret.is_empty() {
        return Err(Error::from("QQ_APP_ID / QQ_APP_SECRET 不能为空"));
    }
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error)
        .with_body(Some(
            json!({"appId": app_id, "clientSecret": secret})
                .to_string()
                .into(),
        ));
    let req = Request::new_with_init("https://bots.qq.com/app/getAppAccessToken", &init)?;
    let (status, value) = crate::http::fetch_json(req, 8).await?;
    if status != 200 {
        return Err(Error::from(format!("QQ 返回 HTTP {status}")));
    }
    if value
        .get("access_token")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        // 验证通过顺手刷新缓存，省一次往返。
        invalidate_token(env).await?;
        Ok(())
    } else {
        let msg = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("返回里没有 access_token");
        Err(Error::from(msg.to_string()))
    }
}

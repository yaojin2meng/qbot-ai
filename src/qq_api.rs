use serde_json::{json, Value};
use worker::*;

fn api_origin(env: &Env) -> Result<String> {
    let origin = env
        .var("QQ_API_BASE")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "https://api.sgroup.qq.com".into());
    let origin = origin.trim_end_matches('/');
    if !["https://api.sgroup.qq.com", "https://api.bot.qq.com"].contains(&origin) {
        return Err(Error::from("QQ_API_BASE must be an official QQ API origin"));
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
    let req = Request::new_with_init(&format!("{}{path}", api_origin(env)?), &init)?;
    let (status, value) = crate::http::fetch_json(req, 10).await?;
    if status == 401 {
        let app_id = env.secret("QQ_APP_ID")?.to_string();
        if let Ok(kv) = env.kv("KV") {
            if kv
                .delete(&format!("qq_access_token:{app_id}"))
                .await
                .is_err()
            {
                console_error!("could not invalidate QQ token cache");
            }
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
    let app_id = env.secret("QQ_APP_ID")?.to_string();
    let secret = env.secret("QQ_APP_SECRET")?.to_string();
    if app_id.trim().is_empty() || secret.is_empty() {
        return Err(Error::from("QQ credentials are empty"));
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
        return Err(Error::from(format!("QQ token endpoint HTTP {status}")));
    }
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::from("QQ token endpoint returned no access_token"))?
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

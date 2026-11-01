use serde::Deserialize;
use worker::*;

const OP_DISPATCH: i64 = 0; // 服务端事件推送
const OP_HTTP_CALLBACK_ACK: i64 = 12; // HTTP 回调模式回包
const OP_CALLBACK_VERIFY: i64 = 13; // 回调地址验证

/// 官方 Payload 结构：{ id, op, d, s, t }
#[derive(serde::Serialize, Deserialize, Clone)]
struct Payload {
    op: i64,
    #[serde(default)]
    d: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    s: Option<i64>,
    #[serde(default)]
    t: Option<String>,
}

/// GROUP_AT_MESSAGE_CREATE 事件体（字段名严格按官方文档）
#[derive(serde::Serialize, Deserialize, Clone)]
pub struct GroupAtMessage {
    pub id: String,
    pub content: String,
    pub group_openid: String,
    pub author: Author,
    #[serde(default)]
    #[allow(dead_code)]
    pub message_scene: Option<serde_json::Value>,
}

#[derive(serde::Serialize, Deserialize, Clone)]
pub struct Author {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub member_openid: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub username: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub member_role: Option<String>,
}

/// INTERACTION_CREATE 事件体（字段名严格按官方文档）
#[derive(serde::Serialize, Deserialize, Clone)]
pub struct InteractionCreate {
    pub id: String, // interaction_id，用于 PUT 回应 + 被动回复
    #[serde(rename = "type", default)]
    pub kind: i64, // 11=消息按钮 12=快捷菜单 13=反馈 ...
    #[serde(default)]
    #[allow(dead_code)]
    pub scene: Option<String>,
    #[serde(default)]
    pub group_openid: Option<String>,
    #[serde(default)]
    pub group_member_openid: Option<String>,
    #[serde(default)]
    pub user_openid: Option<String>,
    #[serde(default)]
    pub data: Option<InteractionData>,
}

#[derive(serde::Serialize, Deserialize, Clone)]
pub struct InteractionData {
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    pub kind: i64,
    #[serde(default)]
    pub resolved: Option<InteractionResolved>,
}

#[derive(serde::Serialize, Deserialize, Clone)]
pub struct InteractionResolved {
    #[serde(default)]
    pub button_data: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub button_id: Option<String>,
}

pub async fn handle_webhook(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let sig_hex = req
        .headers()
        .get("X-Signature-Ed25519")?
        .unwrap_or_default();
    let ts = req
        .headers()
        .get("X-Signature-Timestamp")?
        .unwrap_or_default();
    let body = req.text().await?;
    if body.len() > 64 * 1024 {
        return Response::error("payload too large", 413);
    }
    let skip = ctx
        .var("SKIP_SIGNATURE")
        .map(|v| v.to_string() == "true")
        .unwrap_or(false);
    if !skip {
        let now = Date::now().as_millis() as i64 / 1000;
        if !timestamp_is_fresh(&ts, now) || !verify_signature(&ctx.env, &sig_hex, &ts, &body)? {
            return Response::error("invalid or expired signature", 401);
        }
    } else {
        console_error!("WARNING: QQ signature verification is disabled");
    }
    let payload: Payload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(_) => return Response::error("bad payload", 400),
    };
    if payload.op == OP_CALLBACK_VERIFY {
        let d = payload.d.as_ref();
        let plain = d
            .and_then(|d| d.get("plain_token"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let event_ts = d
            .and_then(|d| d.get("event_ts"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if plain.is_empty() || event_ts.is_empty() {
            return Response::error("invalid challenge", 400);
        }
        return Response::from_json(&serde_json::json!({
            "plain_token": plain,
            "signature": sign_validation(&ctx.env, event_ts, plain)?
        }));
    }
    if payload.op == OP_DISPATCH {
        let d = payload.d.unwrap_or(serde_json::Value::Null);
        let job = match payload.t.as_deref().unwrap_or("") {
            "GROUP_AT_MESSAGE_CREATE" => {
                let event = match serde_json::from_value::<GroupAtMessage>(d) {
                    Ok(e)
                        if crate::qq_api::valid_id(&e.id)
                            && crate::qq_api::valid_id(&e.group_openid) =>
                    {
                        e
                    }
                    _ => return Response::error("invalid group event", 400),
                };
                Some(EventJob::Group { event })
            }
            "INTERACTION_CREATE" => {
                let event: InteractionCreate = match serde_json::from_value(d) {
                    Ok(e) => e,
                    Err(_) => return Response::error("invalid interaction event", 400),
                };
                if !crate::qq_api::valid_id(&event.id) {
                    return Response::error("invalid interaction id", 400);
                }
                if event.kind == 11 || event.kind == 12 {
                    if crate::qq_api::ack_interaction(&ctx.env, &event.id)
                        .await
                        .is_err()
                    {
                        console_error!("interaction acknowledgement failed");
                        // Redeliveries may report 'already acknowledged'; still enqueue the business event.
                    }
                }
                if event.kind == 11 {
                    let event_id = payload.id.unwrap_or_default();
                    if !crate::qq_api::valid_id(&event_id) {
                        return Response::error("missing outer event id", 400);
                    }
                    Some(EventJob::Interaction { event, event_id })
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(job) = job {
            // Durable enqueue before ACK. Enqueue failure must be retried by QQ.
            if ctx.env.queue("EVENTS")?.send(job).await.is_err() {
                console_error!("QQ event enqueue failed");
                return Response::error("queue unavailable", 503);
            }
        }
    }
    Response::from_json(&serde_json::json!({ "op": OP_HTTP_CALLBACK_ACK }))
}

#[derive(serde::Serialize, Deserialize, Clone)]
#[serde(tag = "kind")]
pub enum EventJob {
    Group {
        event: GroupAtMessage,
    },
    Interaction {
        event: InteractionCreate,
        event_id: String,
    },
}

pub async fn process_job(job: &EventJob, env: &Env) -> Result<()> {
    let key = match job {
        EventJob::Group { event } => format!("gmsg:{}:{}", event.group_openid, event.id),
        EventJob::Interaction { event, .. } => format!("intr:{}", event.id),
    };
    let db = env.d1("DB")?;
    if db
        .prepare("SELECT event_key FROM webhook_events WHERE event_key=?1")
        .bind(&[key.clone().into()])?
        .first::<String>(Some("event_key"))
        .await?
        .is_some()
    {
        return Ok(());
    }
    let owner = crate::security::acquire(env, &key, 600)
        .await?
        .ok_or_else(|| Error::from("event is already processing"))?;
    let result = async {
        if db
            .prepare("SELECT event_key FROM webhook_events WHERE event_key=?1")
            .bind(&[key.clone().into()])?
            .first::<String>(Some("event_key"))
            .await?
            .is_some()
        {
            return Ok(());
        }
        match job {
            EventJob::Group { event } => handle_group_message(event, env).await?,
            EventJob::Interaction { event, event_id } => {
                handle_interaction(event, event_id, env).await?
            }
        }
        db.prepare(
            "INSERT OR IGNORE INTO webhook_events (event_key, completed_at) VALUES (?1, ?2)",
        )
        .bind(&[key.clone().into(), (Date::now().as_millis() as i64).into()])?
        .run()
        .await?;
        if rand::random::<u8>() % 32 == 0 {
            db.prepare("DELETE FROM webhook_events WHERE completed_at < ?1")
                .bind(&[(Date::now().as_millis() as i64 - 7 * 86400000).into()])?
                .run()
                .await?;
            db.prepare("DELETE FROM task_leases WHERE expires_at < ?1")
                .bind(&[(Date::now().as_millis() as i64 - 86400000).into()])?
                .run()
                .await?;
        }
        Ok(())
    }
    .await;
    if crate::security::release(env, &key, &owner).await.is_err() {
        console_error!("event lease release failed");
    }
    result
}

fn timestamp_is_fresh(ts: &str, now: i64) -> bool {
    ts.parse::<i64>()
        .map(|ts| now.abs_diff(ts) <= 300)
        .unwrap_or(false)
}

fn verify_signature(env: &Env, sig_hex: &str, ts: &str, body: &str) -> Result<bool> {
    let seed = build_seed(&env.secret("QQ_APP_SECRET")?.to_string())?;
    let bytes = match hex::decode(sig_hex) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(false),
    };
    let sig = match ed25519_dalek::Signature::from_slice(&bytes) {
        Ok(sig) => sig,
        Err(_) => return Ok(false),
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
    Ok(key
        .verify_strict(format!("{ts}{body}").as_bytes(), &sig)
        .is_ok())
}

fn sign_validation(env: &Env, ts: &str, plain: &str) -> Result<String> {
    use ed25519_dalek::Signer;
    let seed = build_seed(&env.secret("QQ_APP_SECRET")?.to_string())?;
    Ok(hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .sign(format!("{ts}{plain}").as_bytes())
            .to_bytes(),
    ))
}

fn build_seed(secret: &str) -> Result<[u8; 32]> {
    if secret.is_empty() {
        return Err(Error::from("QQ_APP_SECRET must not be empty"));
    }
    let mut seed = [0; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = secret.as_bytes()[i % secret.len()];
    }
    Ok(seed)
}

fn strip_mention(text: &str) -> &str {
    let text = text.trim();
    if text.starts_with("<@") {
        if let Some(end) = text.find('>') {
            return text[end + 1..].trim();
        }
    }
    text
}

async fn handle_group_message(event: &GroupAtMessage, env: &Env) -> Result<()> {
    let text = strip_mention(&event.content);
    if let Some((topic, count)) = crate::poll::parse_vote_command(text) {
        return crate::poll::create_poll(env, &event.group_openid, &event.id, &topic, count).await;
    }
    if let Some((base, key)) = crate::model_test::extract_api_and_key(text) {
        let lease_key = format!("group-test:{}", event.group_openid);
        if let Some(owner) = crate::security::acquire(env, &lease_key, 600).await? {
            let result = crate::model_test::test_and_reply_group(
                env,
                &event.group_openid,
                &event.id,
                &base,
                &key,
            )
            .await;
            if result.is_err() {
                crate::security::release(env, &lease_key, &owner).await?;
            }
            return result;
        }
        crate::qq_api::send_group(env, &event.group_openid, serde_json::json!({
            "msg_type": 0, "content": "本群测速正在进行或处于冷却期，请 10 分钟后重试。", "msg_id": event.id, "msg_seq": 1
        })).await?;
    }
    Ok(())
}

async fn handle_interaction(event: &InteractionCreate, event_id: &str, env: &Env) -> Result<()> {
    let user = event
        .group_member_openid
        .as_deref()
        .or(event.user_openid.as_deref())
        .unwrap_or("");
    let group = event.group_openid.as_deref().unwrap_or("");
    if !crate::qq_api::valid_id(user) || !crate::qq_api::valid_id(group) {
        return Ok(());
    }
    let data = event
        .data
        .as_ref()
        .and_then(|d| d.resolved.as_ref())
        .and_then(|r| r.button_data.as_deref())
        .unwrap_or("");
    crate::poll::handle_vote_click(env, group, user, data, event_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    #[test]
    fn official_signature_example() {
        let seed = build_seed("DG5g3B4j9X2KOErG").unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(hex::encode(key.sign(b"1725442341Arq0D5A61EgUu4OxUvOp").to_bytes()),
            "87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706");
    }
    #[test]
    fn timestamps_and_mentions() {
        assert!(timestamp_is_fresh("1000", 1300));
        assert!(!timestamp_is_fresh("1000", 1301));
        assert!(!timestamp_is_fresh("bad", 1300));
        assert_eq!(strip_mention("<@!123> 投票 测试 2"), "投票 测试 2");
    }
}

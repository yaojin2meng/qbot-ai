use worker::*;
use serde::Deserialize;

const OP_DISPATCH: i64 = 0;          // 服务端事件推送
const OP_HTTP_CALLBACK_ACK: i64 = 12; // HTTP 回调模式回包
const OP_CALLBACK_VERIFY: i64 = 13;   // 回调地址验证

/// 官方 Payload 结构：{ id, op, d, s, t }
#[derive(Deserialize)]
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
#[derive(Deserialize)]
pub struct GroupAtMessage {
    pub id: String,
    pub content: String,
    pub group_openid: String,
    pub author: Author,
    #[serde(default)]
    #[allow(dead_code)]
    pub message_scene: Option<serde_json::Value>,
}

#[derive(Deserialize)]
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
#[derive(Deserialize)]
pub struct InteractionCreate {
    pub id: String,                    // interaction_id，用于 PUT 回应 + 被动回复
    #[serde(rename = "type", default)]
    pub kind: i64,                     // 11=消息按钮 12=快捷菜单 13=反馈 ...
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

#[derive(Deserialize)]
pub struct InteractionData {
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    pub kind: i64,
    #[serde(default)]
    pub resolved: Option<InteractionResolved>,
}

#[derive(Deserialize)]
pub struct InteractionResolved {
    #[serde(default)]
    pub button_data: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub button_id: Option<String>,
}

pub async fn handle_webhook(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    // ---------- 1. 读取 body + header ----------
    let sig_hex = req.headers().get("X-Signature-Ed25519")?.unwrap_or_default();
    let ts = req.headers().get("X-Signature-Timestamp")?.unwrap_or_default();
    let body = req.text().await?;

    // ---------- 2. 签名校验（Ed25519）----------
    // 官方：msg = timestamp + body；seed = AppSecret 重复至 32 字节
    // 开发阶段可设变量 SKIP_SIGNATURE=true 跳过（生产不建议）
    let skip = ctx
        .var("SKIP_SIGNATURE")
        .map(|v| v.to_string())
        .unwrap_or_default()
        == "true";
    if !skip {
        if sig_hex.is_empty() || ts.is_empty() {
            return Response::error("missing signature headers", 401);
        }
        if !verify_signature(&ctx, &sig_hex, &ts, &body)? {
            return Response::error("invalid signature", 401);
        }
    }

    let payload: Payload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(_) => return Response::error("bad payload", 400),
    };

    // ---------- 3. 回调地址验证 op=13 ----------
    // 官方要求返回 { plain_token, signature }，signature = hex(Ed25519(event_ts + plain_token))
    if payload.op == OP_CALLBACK_VERIFY {
        if let Some(d) = &payload.d {
            let plain = d.get("plain_token").and_then(|v| v.as_str()).unwrap_or("");
            let event_ts = d.get("event_ts").and_then(|v| v.as_str()).unwrap_or("");
            let signature = sign_validation(&ctx, event_ts, plain)?;
            return Response::from_json(&serde_json::json!({
                "plain_token": plain,
                "signature": signature
            }));
        }
    }

    // ---------- 4. 事件派发 op=0 ----------
    if payload.op == OP_DISPATCH {
        let event_type = payload.t.clone().unwrap_or_default();
        if let Some(d) = payload.d {
            match event_type.as_str() {
                "GROUP_AT_MESSAGE_CREATE" => {
                    if let Ok(evt) = serde_json::from_value::<GroupAtMessage>(d) {
                        // 相同 msg_id 可能重复推送，用 KV 去重（5 分钟）
                        if !is_duplicate(&ctx, &format!("gmsg:{}", evt.id)).await {
                            let _ = handle_group_message(evt, &ctx).await;
                        }
                    }
                }
                "INTERACTION_CREATE" => {
                    if let Ok(evt) = serde_json::from_value::<InteractionCreate>(d) {
                        // 相同 interaction id 只处理一次
                        if !is_duplicate(&ctx, &format!("intr:{}", evt.id)).await {
                            let _ = handle_interaction(evt, &ctx).await;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ---------- 5. HTTP 回调 ACK (op=12) ----------
    Response::from_json(&serde_json::json!({ "op": OP_HTTP_CALLBACK_ACK }))
}

/// 校验 Ed25519 签名
fn verify_signature(ctx: &RouteContext<()>, sig_hex: &str, ts: &str, body: &str) -> Result<bool> {
    use ed25519_dalek::{Signature, Verifier};

    let secret = ctx.secret("QQ_APP_SECRET")?.to_string();
    let seed = build_seed(&secret);

    let mut msg = Vec::new();
    msg.extend_from_slice(ts.as_bytes());
    msg.extend_from_slice(body.as_bytes());

    let sig_bytes = match hex::decode(sig_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return Ok(false),
    };
    let sig_arr: [u8; 64] = match sig_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return Ok(false),
    };
    let sig = Signature::from_bytes(&sig_arr);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();
    Ok(verifying_key.verify(&msg, &sig).is_ok())
}

/// 生成回调地址验证签名：hex(Ed25519(event_ts + plain_token))
fn sign_validation(ctx: &RouteContext<()>, event_ts: &str, plain_token: &str) -> Result<String> {
    use ed25519_dalek::Signer;

    let secret = ctx.secret("QQ_APP_SECRET")?.to_string();
    let seed = build_seed(&secret);
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);

    let mut msg = Vec::new();
    msg.extend_from_slice(event_ts.as_bytes());
    msg.extend_from_slice(plain_token.as_bytes());
    let sig = signing_key.sign(&msg);
    Ok(hex::encode(sig.to_bytes()))
}

/// AppSecret 重复拼接至 32 字节作为 seed（严格按官方 Go 示例逻辑）
fn build_seed(secret: &str) -> [u8; 32] {
    let mut s = secret.to_string();
    while s.len() < 32 {
        s = format!("{}{}", s, s);
    }
    let bytes = s.as_bytes();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes[..32]);
    seed
}

/// KV 去重：键已存在返回 true；否则写入并返回 false
async fn is_duplicate(ctx: &RouteContext<()>, key: &str) -> bool {
    if let Ok(kv) = ctx.env.kv("KV") {
        if let Ok(Some(_)) = kv.get(key).text().await {
            return true;
        }
        let _ = kv
            .put(key, "1")
            .and_then(|b| b.expiration_ttl(300))
            .and_then(|b| b.execute())
            .await;
    }
    false
}

/// 处理群 @消息：识别「投票 <主题> <人数>」指令，或识别 API+Key 自动测速出图
async fn handle_group_message(evt: GroupAtMessage, ctx: &RouteContext<()>) -> Result<()> {
    let text = evt.content.trim().to_string(); // 官方已自动去除 @ 前缀

    // 1. 检查是否为投票指令
    if let Some((topic, target)) = crate::poll::parse_vote_command(&text) {
        let member = evt
            .author
            .member_openid
            .clone()
            .unwrap_or_else(|| evt.author.id.clone());
        crate::poll::create_poll(ctx, &evt.group_openid, &evt.id, &member, &topic, target).await?;
        return Ok(());
    }

    // 2. 检查是否为 API + Key 配置（格式一: NewAPI JSON, 格式二: 换行/包含 url+key 的文本）
    if let Some((url, key)) = crate::model_test::extract_api_and_key(&text) {
        crate::model_test::test_and_reply_group(ctx, &evt.group_openid, &evt.id, &url, &key).await?;
        return Ok(());
    }

    Ok(())
}

/// 处理按钮点击：必须先 PUT 回应 ACK，再执行业务
async fn handle_interaction(evt: InteractionCreate, ctx: &RouteContext<()>) -> Result<()> {
    // 仅 type=11（消息按钮）需要 PUT 回应，否则客户端一直 loading
    if evt.kind == 11 {
        let _ = crate::poll::ack_interaction(ctx, &evt.id).await;
    } else {
        return Ok(());
    }

    let user = evt
        .group_member_openid
        .clone()
        .or(evt.user_openid.clone())
        .unwrap_or_default();
    let group = evt.group_openid.clone().unwrap_or_default();

    let btn_data = evt
        .data
        .as_ref()
        .and_then(|d| d.resolved.as_ref())
        .and_then(|r| r.button_data.clone())
        .unwrap_or_default();

    if !btn_data.is_empty() {
        crate::poll::handle_vote_click(ctx, &group, &user, &btn_data, &evt.id).await?;
    }
    Ok(())
}
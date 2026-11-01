use worker::*;
use serde_json::json;

/// 解析指令：`投票 <主题> <预计人数>`
/// 例如：投票 周末去爬山吗 10
pub fn parse_vote_command(text: &str) -> Option<(String, i64)> {
    let t = text.trim();
    let rest = t.strip_prefix("投票")?;
    let rest = rest.trim();
    // 最后一个空白后是人数
    let idx = rest.rfind(char::is_whitespace)?;
    let topic = rest[..idx].trim().to_string();
    let count: i64 = rest[idx..].trim().parse().ok()?;
    if topic.is_empty() || count <= 0 {
        return None;
    }
    Some((topic, count))
}

fn gen_id() -> String {
    let now = Date::now().as_millis();
    let r: u32 = rand::random();
    format!("{:x}{:08x}", now, r)
}

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

/// 创建投票：写库 -> 发出带三按钮的 Markdown 消息
pub async fn create_poll(
    ctx: &RouteContext<()>,
    group_id: &str,
    msg_id: &str,
    _creator: &str,
    topic: &str,
    target: i64,
) -> Result<()> {
    let db = ctx.env.d1("DB")?;
    let poll_id = gen_id();

    db.prepare(
        "INSERT INTO polls (poll_id, group_id, topic, target_count, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
    )
    .bind(&[
        poll_id.clone().into(),
        group_id.into(),
        topic.into(),
        target.into(),
        now_ms().into(),
    ])?
    .run()
    .await?;

    // 官方结构：msg_type=2 时内容放 markdown.content，content 必须为空
    // 按钮 action.type=1 表示「回调按钮」，点击后 data 回传后台
    let payload = json!({
        "msg_type": 2,
        "markdown": {
            "content": format!(
                "# 📊 投票开始\n\n> {}\n\n预计参与人数：**{}** 人\n\n点击下方按钮投票，每人限一票。",
                topic, target
            )
        },
        "keyboard": {
            "content": {
                "rows": [
                    { "buttons": [
                        {
                            "id": "agree",
                            "render_data": { "label": "赞成", "visited_label": "已赞成", "style": 1 },
                            "action": { "type": 1, "permission": { "type": 2 }, "data": format!("vote:{}:agree", poll_id) }
                        },
                        {
                            "id": "disagree",
                            "render_data": { "label": "反对", "visited_label": "已反对", "style": 1 },
                            "action": { "type": 1, "permission": { "type": 2 }, "data": format!("vote:{}:disagree", poll_id) }
                        },
                        {
                            "id": "abstain",
                            "render_data": { "label": "弃权", "visited_label": "已弃权", "style": 1 },
                            "action": { "type": 1, "permission": { "type": 2 }, "data": format!("vote:{}:abstain", poll_id) }
                        }
                    ]}
                ]
            }
        },
        "msg_id": msg_id,
        "msg_seq": 1
    });

    send_group_payload(ctx, group_id, payload).await?;
    Ok(())
}

/// 回应 INTERACTION_CREATE（必须调用，否则客户端一直 loading）
/// 官方：PUT /interactions/{interaction_id}，body { code }
pub async fn ack_interaction(ctx: &RouteContext<()>, interaction_id: &str) -> Result<i64> {
    let token = get_access_token(ctx).await?;
    let url = format!("https://api.sgroup.qq.com/interactions/{}", interaction_id);

    let headers = Headers::new();
    headers.set("Authorization", &format!("QQBot {}", token))?;
    headers.set("Content-Type", "application/json")?;

    let mut init = RequestInit::new();
    init.with_method(Method::Put)
        .with_headers(headers)
        .with_body(Some(json!({ "code": 0 }).to_string().into()));

    let req = Request::new_with_init(&url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let ok = resp.status_code() == 200;
    Ok(if ok { 0 } else { 1 })
}

/// 处理按钮点击：去重写入 -> 累加 -> 判断是否达成目标人数 -> 结算
pub async fn handle_vote_click(
    ctx: &RouteContext<()>,
    group_id: &str,
    user_id: &str,
    btn_data: &str,
    interaction_id: &str,
) -> Result<()> {
    let mut parts = btn_data.split(':');
    if parts.next() != Some("vote") {
        return Ok(());
    }
    let poll_id = match parts.next() {
        Some(p) => p.to_string(),
        None => return Ok(()),
    };
    let choice = match parts.next() {
        Some(c) => c.to_string(),
        None => return Ok(()),
    };
    if !["agree", "disagree", "abstain"].contains(&choice.as_str()) {
        return Ok(());
    }

    let db = ctx.env.d1("DB")?;

    // 一人一票：INSERT OR IGNORE，主键 (poll_id,user_id) 冲突时影响行数为 0
    let res = db
        .prepare(
            "INSERT OR IGNORE INTO poll_votes (poll_id, user_id, choice, created_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&[
            poll_id.clone().into(),
            user_id.into(),
            choice.clone().into(),
            now_ms().into(),
        ])?
        .run()
        .await?;

    let inserted = res.meta()?.map(|m| m.changes).unwrap_or(0);
    if inserted == 0 {
        return Ok(()); // 已投过票，静默忽略
    }

    let col = match choice.as_str() {
        "agree" => "agree",
        "disagree" => "disagree",
        _ => "abstain",
    };
    db.prepare(&format!(
        "UPDATE polls SET {col} = {col} + 1 WHERE poll_id = ?1 AND status = 'active'"
    ))
    .bind(&[poll_id.clone().into()])?
    .run()
    .await?;

    let row = db
        .prepare("SELECT agree, disagree, abstain, target_count, topic, status FROM polls WHERE poll_id = ?1")
        .bind(&[poll_id.clone().into()])?
        .first::<PollRow>(None)
        .await?;

    if let Some(p) = row {
        let total = p.agree + p.disagree + p.abstain;
        if p.status == "active" && total >= p.target_count {
            db.prepare("UPDATE polls SET status='closed', closed_at=?2 WHERE poll_id=?1 AND status='active'")
                .bind(&[poll_id.clone().into(), now_ms().into()])?
                .run()
                .await?;

            let summary = format!(
                "# ✅ 投票结束：{}\n\n- 👍 赞成：**{}**\n- 👎 反对：**{}**\n- 🤷 弃权：**{}**\n\n共 **{}** 人参与，已达预计人数 **{}**。",
                p.topic, p.agree, p.disagree, p.abstain, total, p.target_count
            );
            // 用 interaction_id 作为 event_id 作被动回复（官方允许 INTERACTION_CREATE 事件回复）
            let payload = json!({
                "msg_type": 2,
                "markdown": { "content": summary },
                "event_id": interaction_id,
                "msg_seq": 1
            });
            send_group_payload(ctx, group_id, payload).await?;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct PollRow {
    agree: i64,
    disagree: i64,
    abstain: i64,
    target_count: i64,
    topic: String,
    status: String,
}

/// 统一发送群消息
async fn send_group_payload(
    ctx: &RouteContext<()>,
    group_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let token = get_access_token(ctx).await?;
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

/// 获取 QQ Bot AccessToken（缓存到 KV，提前 60s 过期）
/// 官方：POST https://bots.qq.com/app/getAppAccessToken  body {appId, clientSecret}
pub async fn get_access_token(ctx: &RouteContext<()>) -> Result<String> {
    let kv = ctx.env.kv("KV")?;
    if let Some(tok) = kv.get("qq_access_token").text().await? {
        if !tok.is_empty() {
            return Ok(tok);
        }
    }

    let app_id = ctx.secret("QQ_APP_ID")?.to_string();
    let secret = ctx.secret("QQ_APP_SECRET")?.to_string();

    let url = "https://bots.qq.com/app/getAppAccessToken";
    let payload = json!({ "appId": app_id, "clientSecret": secret });

    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(payload.to_string().into()));

    let req = Request::new_with_init(url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let v: serde_json::Value = resp.json().await?;

    let token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // expires_in 官方返回为字符串
    let expires = v
        .get("expires_in")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(7200);

    if !token.is_empty() {
        let _ = kv
            .put("qq_access_token", token.clone())?
            .expiration_ttl((expires - 60).max(60) as u64)
            .execute()
            .await;
    }
    Ok(token)
}
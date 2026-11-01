use serde_json::json;
use worker::*;

/// 解析指令：`投票 <主题> <预计人数>`
/// 例如：投票 周末去爬山吗 10
pub fn parse_vote_command(text: &str) -> Option<(String, i64)> {
    let t = text.trim();
    let rest = t.strip_prefix("投票")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    // 最后一个空白后是人数
    let idx = rest.rfind(char::is_whitespace)?;
    let topic = rest[..idx].trim().to_string();
    let count: i64 = rest[idx..].trim().parse().ok()?;
    if topic.is_empty() || topic.chars().count() > 200 || !(1..=10000).contains(&count) {
        return None;
    }
    Some((topic, count))
}

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}

/// 创建投票：写库 -> 发出带三按钮的 Markdown 消息
pub async fn create_poll(
    env: &Env,
    group_id: &str,
    msg_id: &str,
    topic: &str,
    target: i64,
) -> Result<()> {
    let db = env.d1("DB")?;
    use sha2::{Digest, Sha256};
    let poll_id =
        hex::encode(Sha256::digest(format!("{group_id}:{msg_id}").as_bytes()))[..32].to_string();
    if db
        .prepare("SELECT message_sent FROM polls WHERE poll_id=?1")
        .bind(&[poll_id.clone().into()])?
        .first::<i64>(Some("message_sent"))
        .await?
        == Some(1)
    {
        return Ok(());
    }

    db.prepare(
        "INSERT OR IGNORE INTO polls (poll_id, group_id, topic, target_count, status, created_at)
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

    crate::qq_api::send_group(env, group_id, payload).await?;
    db.prepare("UPDATE polls SET message_sent=1 WHERE poll_id=?1")
        .bind(&[poll_id.into()])?
        .run()
        .await?;
    Ok(())
}

/// Vote insertion, counters and closing are atomic via migration 0002 triggers.
pub async fn handle_vote_click(
    env: &Env,
    group_id: &str,
    user_id: &str,
    btn_data: &str,
    event_id: &str,
) -> Result<()> {
    if !crate::qq_api::valid_id(group_id) || !crate::qq_api::valid_id(user_id) {
        return Ok(());
    }
    let parts: Vec<_> = btn_data.split(':').collect();
    if parts.len() != 3
        || parts[0] != "vote"
        || !crate::qq_api::valid_id(parts[1])
        || !["agree", "disagree", "abstain"].contains(&parts[2])
    {
        return Ok(());
    }
    let (poll_id, choice) = (parts[1], parts[2]);
    let db = env.d1("DB")?;
    db.prepare(
        "INSERT OR IGNORE INTO poll_votes (poll_id, user_id, choice, created_at, event_id)
         SELECT poll_id, ?2, ?3, ?4, ?5 FROM polls
         WHERE poll_id=?1 AND group_id=?6 AND status='active'",
    )
    .bind(&[
        poll_id.into(),
        user_id.into(),
        choice.into(),
        now_ms().into(),
        event_id.into(),
        group_id.into(),
    ])?
    .run()
    .await?;
    let row = db.prepare(
        "SELECT agree, disagree, abstain, target_count, topic, status, summary_event_id, summary_sent
         FROM polls WHERE poll_id=?1 AND group_id=?2"
    ).bind(&[poll_id.into(), group_id.into()])?.first::<PollRow>(None).await?;
    if let Some(p) = row {
        // Only the closing event may send/retry this summary, even on duplicate votes.
        if p.status == "closed"
            && p.summary_sent == 0
            && p.summary_event_id.as_deref() == Some(event_id)
        {
            let total = p.agree + p.disagree + p.abstain;
            let summary = format!(
                "# 投票结束：{}\n\n- 赞成：**{}**\n- 反对：**{}**\n- 弃权：**{}**\n\n共 **{}** 人参与，目标 **{}** 人。",
                p.topic, p.agree, p.disagree, p.abstain, total, p.target_count
            );
            crate::qq_api::send_group(env, group_id, json!({
                "msg_type": 2, "markdown": {"content": summary}, "event_id": event_id, "msg_seq": 1
            })).await?;
            db.prepare("UPDATE polls SET summary_sent=1 WHERE poll_id=?1")
                .bind(&[poll_id.into()])?
                .run()
                .await?;
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
    summary_event_id: Option<String>,
    summary_sent: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands() {
        assert_eq!(
            parse_vote_command("投票 周末 去爬山 10"),
            Some(("周末 去爬山".into(), 10))
        );
        for text in ["投票", "投票 A 0", "投票 A -1", "投票 A 10001", "投票A 2"] {
            assert!(parse_vote_command(text).is_none(), "{text}");
        }
    }
}

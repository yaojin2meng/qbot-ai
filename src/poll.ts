import type { Env } from './env';
import { sendGroup, validId } from './qqApi';

const CHOICES = ['agree', 'disagree', 'abstain'] as const;
type Choice = (typeof CHOICES)[number];

export interface PollRow {
  agree: number;
  disagree: number;
  abstain: number;
  target_count: number;
  topic: string;
  status: string;
  summary_event_id: string | null;
  summary_sent: number;
}

/**
 * 解析指令：`投票 <主题> <预计人数>`
 * 例：投票 周末去爬山吗 10
 */
export function parseVoteCommand(text: string): { topic: string; count: number } | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith('投票')) return null;
  const rest = trimmed.slice(2);
  if (rest.length === 0 || !/^\s/.test(rest)) return null;

  const body = rest.trim();
  const match = /^(.*\S)\s+(\d+)$/s.exec(body);
  if (!match) return null;

  const topic = (match[1] ?? '').trim();
  const count = Number.parseInt(match[2] ?? '', 10);
  if (topic.length === 0 || [...topic].length > 200) return null;
  if (!Number.isFinite(count) || count < 1 || count > 10000) return null;
  return { topic, count };
}

async function sha256Hex(input: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * 创建投票：写库 → 发出带三按钮的 Markdown 消息。
 * poll_id 由 (group_id, msg_id) 派生，天然幂等。
 */
export async function createPoll(
  env: Env,
  groupId: string,
  msgId: string,
  topic: string,
  target: number,
): Promise<void> {
  const pollId = (await sha256Hex(`${groupId}:${msgId}`)).slice(0, 32);

  const existing = await env.DB.prepare('SELECT message_sent FROM polls WHERE poll_id=?1')
    .bind(pollId)
    .first<{ message_sent: number }>();
  if (existing?.message_sent === 1) return;

  await env.DB.prepare(
    `INSERT OR IGNORE INTO polls (poll_id, group_id, topic, target_count, status, created_at)
     VALUES (?1, ?2, ?3, ?4, 'active', ?5)`,
  )
    .bind(pollId, groupId, topic, target, Date.now())
    .run();

  // msg_type=2 时内容放 markdown.content，顶层 content 必须为空；
  // 按钮 action.type=1 表示「回调按钮」，点击后 data 会回传到后台。
  const payload = {
    msg_type: 2,
    markdown: {
      content: `# 📊 投票开始\n\n> ${topic}\n\n预计参与人数：**${target}** 人\n\n点击下方按钮投票，每人限一票。`,
    },
    keyboard: {
      content: {
        rows: [
          {
            buttons: [
              button('agree', '赞成', '已赞成', `vote:${pollId}:agree`),
              button('disagree', '反对', '已反对', `vote:${pollId}:disagree`),
              button('abstain', '弃权', '已弃权', `vote:${pollId}:abstain`),
            ],
          },
        ],
      },
    },
    msg_id: msgId,
    msg_seq: 1,
  };

  await sendGroup(env, groupId, payload);
  await env.DB.prepare('UPDATE polls SET message_sent=1 WHERE poll_id=?1').bind(pollId).run();
}

function button(id: Choice, label: string, visited: string, data: string) {
  return {
    id,
    render_data: { label, visited_label: visited, style: 1 },
    action: { type: 1, permission: { type: 2 }, data },
  };
}

/**
 * 处理按钮点击。
 *
 * 落票 + 计数 + 关票在同一个 D1 事务（batch，全局原子）里完成：
 * 1) 只有当票还开着、且还没满员时，INSERT 才会生效（INSERT OR IGNORE + WHERE）；
 * 2) 紧接着从明细表 poll_votes 重算三个计数——明细是唯一真相，
 *    所以不管来多少重复/并发请求，计数都不会漂；
 * 3) 重算的同时判断是否满员，满了就把 status 切到 closed，
 *    并把「是谁把票投满的」记进 summary_event_id。
 * 以前这套逻辑靠 SQLite 触发器，改成重算后不依赖 D1 对触发器的支持。
 */
export async function handleVoteClick(
  env: Env,
  groupId: string,
  userId: string,
  buttonData: string,
  eventId: string,
): Promise<void> {
  if (!validId(groupId) || !validId(userId)) return;

  const parts = buttonData.split(':');
  if (parts.length !== 3 || parts[0] !== 'vote') return;
  const pollId = parts[1] ?? '';
  const choice = parts[2] ?? '';
  if (!validId(pollId) || !CHOICES.includes(choice as Choice)) return;

  const now = Date.now();
  const counts = (choice: Choice) =>
    `(SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND v.choice='${choice}')`;

  await env.DB.batch([
    env.DB.prepare(
      `INSERT OR IGNORE INTO poll_votes (poll_id, user_id, choice, created_at, event_id)
       SELECT poll_id, ?2, ?3, ?4, ?5 FROM polls
       WHERE poll_id=?1 AND group_id=?6 AND status='active'
         AND target_count > (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id)`,
    ).bind(pollId, userId, choice, now, eventId, groupId),
    env.DB.prepare(
      `UPDATE polls SET
         agree    = ${counts('agree')},
         disagree = ${counts('disagree')},
         abstain  = ${counts('abstain')},
         status   = CASE WHEN status='active'
                          AND ${counts('agree')} + ${counts('disagree')} + ${counts('abstain')}
                              >= target_count
                         THEN 'closed' ELSE status END,
         closed_at        = CASE WHEN status='active'
                                  AND ${counts('agree')} + ${counts('disagree')} + ${counts('abstain')}
                                      >= target_count
                                 THEN ?2 ELSE closed_at END,
         summary_event_id = CASE WHEN status='active'
                                  AND ${counts('agree')} + ${counts('disagree')} + ${counts('abstain')}
                                      >= target_count
                                 THEN ?3 ELSE summary_event_id END
       WHERE poll_id=?1 AND group_id=?4`,
    ).bind(pollId, now, eventId, groupId),
  ]);

  const row = await env.DB.prepare(
    `SELECT agree, disagree, abstain, target_count, topic, status, summary_event_id, summary_sent
     FROM polls WHERE poll_id=?1 AND group_id=?2`,
  )
    .bind(pollId, groupId)
    .first<PollRow>();

  // 只有「触发结算的那个事件」能发汇总，重复投递也不会重发。
  if (!row) return;
  if (row.status !== 'closed' || row.summary_sent !== 0) return;
  if (row.summary_event_id !== eventId) return;

  const total = row.agree + row.disagree + row.abstain;
  const summary =
    `# 投票结束：${row.topic}\n\n- 赞成：**${row.agree}**\n- 反对：**${row.disagree}**\n` +
    `- 弃权：**${row.abstain}**\n\n共 **${total}** 人参与，目标 **${row.target_count}** 人。`;

  await sendGroup(env, groupId, {
    msg_type: 2,
    markdown: { content: summary },
    event_id: eventId,
    msg_seq: 1,
  });
  await env.DB.prepare('UPDATE polls SET summary_sent=1 WHERE poll_id=?1').bind(pollId).run();
}
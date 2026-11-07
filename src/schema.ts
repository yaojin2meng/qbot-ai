/**
 * 数据库结构的唯一来源。
 *
 * 以前靠 `migrations/*.sql` + 手动跑 `wrangler d1 migrations apply`，
 * 少跑一步就是「表 0」，然后加表、投票全报错。现在改成 Worker 自己在
 * 第一次请求时把表建起来（见 db.ts），不需要任何人工步骤。
 *
 * 约定：
 * - CREATE_* 全部带 IF NOT EXISTS，可以重复执行。
 * - ADD_COLUMN_* 给「已经跑过旧 migration」的库补列，
 *   在新库上会报 duplicate column，被 db.ts 忽略。
 * - 改结构就改这里，并把 SCHEMA_VERSION +1。
 *
 * 注意：这里**不用触发器**。计数改成投票时在同一事务里从明细表重算
 * （poll.ts），所以不依赖 D1 对 CREATE TRIGGER 的支持，重算结果也永远
 * 和 poll_votes 一致，不会出现「关票了但计数没动」。
 */

export const SCHEMA_VERSION = 1;

/** 建表 / 建索引。 */
export const CREATE_STATEMENTS: readonly string[] = [
  `CREATE TABLE IF NOT EXISTS polls (
     poll_id           TEXT PRIMARY KEY,
     group_id          TEXT NOT NULL,
     channel_id        TEXT,
     topic             TEXT NOT NULL,
     target_count      INTEGER NOT NULL,
     agree             INTEGER NOT NULL DEFAULT 0,
     disagree          INTEGER NOT NULL DEFAULT 0,
     abstain           INTEGER NOT NULL DEFAULT 0,
     msg_id            TEXT,
     status            TEXT NOT NULL DEFAULT 'active',
     created_at        INTEGER NOT NULL,
     closed_at         INTEGER,
     message_sent      INTEGER NOT NULL DEFAULT 0,
     summary_event_id  TEXT,
     summary_sent      INTEGER NOT NULL DEFAULT 0
   )`,
  `CREATE INDEX IF NOT EXISTS idx_polls_group ON polls(group_id, status)`,
  `CREATE TABLE IF NOT EXISTS poll_votes (
     poll_id    TEXT NOT NULL,
     user_id    TEXT NOT NULL,
     choice     TEXT NOT NULL,
     created_at INTEGER NOT NULL,
     event_id   TEXT,
     PRIMARY KEY (poll_id, user_id)
   )`,
  `CREATE TABLE IF NOT EXISTS webhook_events (
     event_key    TEXT PRIMARY KEY,
     completed_at INTEGER NOT NULL
   )`,
  `CREATE INDEX IF NOT EXISTS idx_webhook_events_completed ON webhook_events(completed_at)`,
  `CREATE TABLE IF NOT EXISTS task_leases (
     lease_key  TEXT PRIMARY KEY,
     owner      TEXT NOT NULL,
     expires_at INTEGER NOT NULL
   )`,
  `CREATE INDEX IF NOT EXISTS idx_task_leases_expires ON task_leases(expires_at)`,
  `CREATE TABLE IF NOT EXISTS settings (
     key        TEXT PRIMARY KEY,
     value      TEXT NOT NULL,
     updated_at INTEGER NOT NULL
   )`,
];

/** 老库补列（等价于原来的 0002 迁移）。新库上会报 duplicate column，忽略。 */
export const ADD_COLUMN_STATEMENTS: readonly string[] = [
  `ALTER TABLE polls ADD COLUMN message_sent INTEGER NOT NULL DEFAULT 0`,
  `ALTER TABLE polls ADD COLUMN summary_event_id TEXT`,
  `ALTER TABLE polls ADD COLUMN summary_sent INTEGER NOT NULL DEFAULT 0`,
  `ALTER TABLE poll_votes ADD COLUMN event_id TEXT`,
];

/**
 * 从明细表重算计数。只在版本升级时跑一次，用来修正历史脏数据
 * （比如旧触发器方案下计数漂了的情况）。
 */
export const REPAIR_STATEMENTS: readonly string[] = [
  `UPDATE polls SET
     agree    = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND v.choice='agree'),
     disagree = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND v.choice='disagree'),
     abstain  = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND v.choice='abstain')`,
];
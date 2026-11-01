-- 投票主表
CREATE TABLE IF NOT EXISTS polls (
  poll_id       TEXT PRIMARY KEY,          -- uuid
  group_id      TEXT NOT NULL,             -- 群 openid
  channel_id    TEXT,                      -- 子频道(若有)
  topic         TEXT NOT NULL,             -- 投票主题
  target_count  INTEGER NOT NULL,          -- 预计投票人数
  agree         INTEGER NOT NULL DEFAULT 0,
  disagree      INTEGER NOT NULL DEFAULT 0,
  abstain       INTEGER NOT NULL DEFAULT 0,
  msg_id        TEXT,                      -- 发出的投票消息 id
  status        TEXT NOT NULL DEFAULT 'active', -- active / closed
  created_at    INTEGER NOT NULL,
  closed_at     INTEGER
);

CREATE INDEX IF NOT EXISTS idx_polls_group ON polls(group_id, status);

-- 投票明细：主键(poll_id,user_id) 天然保证一人一票
CREATE TABLE IF NOT EXISTS poll_votes (
  poll_id    TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  choice     TEXT NOT NULL,               -- agree / disagree / abstain
  created_at INTEGER NOT NULL,
  PRIMARY KEY (poll_id, user_id)
);

-- 模型 API 配置
CREATE TABLE IF NOT EXISTS api_configs (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT NOT NULL,              -- 自定义名称
  base_url    TEXT NOT NULL,              -- 如 https://api.openai.com/v1
  api_key     TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

-- 模型测试记录
CREATE TABLE IF NOT EXISTS model_tests (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  config_id   INTEGER NOT NULL,
  model       TEXT NOT NULL,
  ok          INTEGER NOT NULL,           -- 1 成功 0 失败
  latency_ms  INTEGER,                    -- 成功时的延迟
  error       TEXT,                       -- 失败原因（超时/错误）
  prompt      TEXT,                       -- 本次使用的随机测试题
  answer      TEXT,                       -- 模型返回（截断）
  created_at  INTEGER NOT NULL,
  FOREIGN KEY(config_id) REFERENCES api_configs(id)
);

CREATE INDEX IF NOT EXISTS idx_tests_config ON model_tests(config_id, created_at);
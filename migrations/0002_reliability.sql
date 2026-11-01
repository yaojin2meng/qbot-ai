-- Additive migration: preserves existing configs, votes and tests.
ALTER TABLE polls ADD COLUMN message_sent INTEGER NOT NULL DEFAULT 0;
ALTER TABLE polls ADD COLUMN summary_event_id TEXT;
ALTER TABLE polls ADD COLUMN summary_sent INTEGER NOT NULL DEFAULT 0;
ALTER TABLE poll_votes ADD COLUMN event_id TEXT;

CREATE TABLE IF NOT EXISTS webhook_events (
  event_key TEXT PRIMARY KEY,
  completed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_events_completed ON webhook_events(completed_at);
CREATE TABLE IF NOT EXISTS task_leases (
  lease_key TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_leases_expires ON task_leases(expires_at);

-- Repair counters from the authoritative vote detail table before installing triggers.
UPDATE polls SET
  agree = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND choice='agree'),
  disagree = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND choice='disagree'),
  abstain = (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id=polls.poll_id AND choice='abstain');

CREATE TRIGGER IF NOT EXISTS poll_vote_validate
BEFORE INSERT ON poll_votes
WHEN NEW.user_id = '' OR NEW.choice NOT IN ('agree','disagree','abstain')
  OR NOT EXISTS (
    SELECT 1 FROM polls WHERE poll_id=NEW.poll_id AND status='active'
      AND agree+disagree+abstain < target_count
  )
BEGIN
  SELECT RAISE(IGNORE);
END;

-- The insert, count update and close transition share one SQLite statement transaction.
CREATE TRIGGER IF NOT EXISTS poll_vote_count
AFTER INSERT ON poll_votes
BEGIN
  UPDATE polls SET
    agree = agree + (NEW.choice='agree'),
    disagree = disagree + (NEW.choice='disagree'),
    abstain = abstain + (NEW.choice='abstain')
  WHERE poll_id=NEW.poll_id;
  UPDATE polls SET status='closed', closed_at=NEW.created_at, summary_event_id=NEW.event_id
  WHERE poll_id=NEW.poll_id AND status='active' AND agree+disagree+abstain >= target_count;
END;
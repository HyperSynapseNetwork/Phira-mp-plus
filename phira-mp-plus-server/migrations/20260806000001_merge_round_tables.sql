-- 合并触控/判定数据：3 张表（touch_batches / judge_batches / player_data）
-- → 1 张 mp_round_player_data（data_uuid 主键 + 嵌套原始批数组，数据量不变）。

-- 1. 新统一表
CREATE TABLE IF NOT EXISTS mp_round_player_data_new (
    data_uuid     TEXT PRIMARY KEY,
    round_uuid    TEXT NOT NULL,
    player_id     INTEGER NOT NULL,
    touches       JSONB NOT NULL DEFAULT '[]'::jsonb,
    judges        JSONB NOT NULL DEFAULT '[]'::jsonb,
    touch_batches JSONB NOT NULL DEFAULT '[]'::jsonb,
    judge_batches JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at    BIGINT NOT NULL,
    updated_at    BIGINT NOT NULL,
    sequence      BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence'),
    UNIQUE(round_uuid, player_id)
);

-- 2. 迁移聚合数据（touches/judges）
INSERT INTO mp_round_player_data_new
    (data_uuid, round_uuid, player_id, touches, judges, created_at, updated_at, sequence)
SELECT gen_random_uuid()::text, round_uuid, player_id, touches, judges, created_at, updated_at, sequence
FROM mp_round_player_data;

-- 3. 迁移原始批为嵌套数组（按 round+player 聚合，保留原 sequence 供增量轮询）
UPDATE mp_round_player_data_new npd SET
    touch_batches = COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'seq', sequence, 'count', count,
            'first_game_time', first_game_time, 'last_game_time', last_game_time,
            'payload', payload
        ))
        FROM mp_round_touch_batches tb
        WHERE tb.round_uuid = npd.round_uuid AND tb.player_id = npd.player_id
    ), '[]'::jsonb),
    judge_batches = COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'seq', sequence, 'count', count,
            'first_game_time', first_game_time, 'last_game_time', last_game_time,
            'payload', payload
        ))
        FROM mp_round_judge_batches jb
        WHERE jb.round_uuid = npd.round_uuid AND jb.player_id = npd.player_id
    ), '[]'::jsonb);

-- 4. 索引（保留原批查询语义）
CREATE INDEX IF NOT EXISTS idx_mp_round_player_data_round_player
    ON mp_round_player_data_new(round_uuid, player_id);
CREATE INDEX IF NOT EXISTS idx_mp_round_player_data_updated
    ON mp_round_player_data_new(updated_at);

-- 5. 替换旧表
DROP TABLE mp_round_touch_batches;
DROP TABLE mp_round_judge_batches;
DROP TABLE mp_round_player_data;
ALTER TABLE mp_round_player_data_new RENAME TO mp_round_player_data;

-- 6. 删除纯写不读的兼容表 room_history（数据在 mp_user_room_history，已并行写入）
DROP TABLE room_history;

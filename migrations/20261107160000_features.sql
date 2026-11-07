ALTER TABLE users ADD COLUMN bio TEXT NOT NULL DEFAULT '' CHECK (char_length(bio) <= 600);
ALTER TABLE users ADD COLUMN status TEXT NOT NULL DEFAULT 'online'
    CHECK (status IN ('online','idle','dnd','invisible'));
ALTER TABLE messages ADD COLUMN pinned_at TIMESTAMPTZ;
ALTER TABLE messages ADD COLUMN pinned_by UUID REFERENCES users(id) ON DELETE SET NULL;

CREATE TABLE channel_reads (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    last_read_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, channel_id)
);

CREATE TABLE notifications (
    id UUID PRIMARY KEY,
    recipient_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('mention', 'direct_message', 'friend_request')),
    channel_id UUID REFERENCES channels(id) ON DELETE CASCADE,
    thread_id UUID REFERENCES direct_threads(id) ON DELETE CASCADE,
    message_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ,
    CHECK ((channel_id IS NULL) OR (thread_id IS NULL))
);

CREATE INDEX notifications_recipient_idx ON notifications(recipient_id,created_at DESC);
CREATE UNIQUE INDEX notifications_unique_message_idx
    ON notifications(recipient_id,kind,message_id) WHERE message_id IS NOT NULL;

CREATE TABLE server_bans (
    server_id UUID NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    moderator_id UUID NOT NULL REFERENCES users(id),
    reason TEXT NOT NULL DEFAULT '' CHECK (char_length(reason) <= 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(server_id,user_id)
);

ALTER TABLE memberships DROP CONSTRAINT IF EXISTS memberships_role_check;
ALTER TABLE memberships ADD CONSTRAINT memberships_role_check
    CHECK (role IN ('owner','moderator','member'));

CREATE INDEX messages_search_idx ON messages USING GIN (to_tsvector('simple',content));
CREATE INDEX messages_pins_idx ON messages(channel_id,pinned_at DESC) WHERE pinned_at IS NOT NULL;

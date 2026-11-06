ALTER TABLE messages ADD COLUMN edited_at TIMESTAMPTZ;
ALTER TABLE messages ADD COLUMN reply_to UUID REFERENCES messages(id) ON DELETE SET NULL;

CREATE TABLE message_reactions (
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('like','agree','laugh','heart','surprised')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (message_id,user_id,kind)
);

CREATE INDEX message_reactions_message_idx ON message_reactions (message_id);

CREATE TABLE friendships (
    user_a UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    requested_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending','accepted')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_a,user_b),
    CHECK (user_a < user_b)
);

CREATE INDEX friendships_user_b_idx ON friendships (user_b);

CREATE TABLE direct_threads (
    id UUID PRIMARY KEY,
    user_a UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_a,user_b),
    CHECK (user_a < user_b)
);

CREATE INDEX direct_threads_user_b_idx ON direct_threads (user_b);

CREATE TABLE direct_messages (
    id UUID PRIMARY KEY,
    thread_id UUID NOT NULL REFERENCES direct_threads(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES users(id),
    content TEXT NOT NULL,
    edited_at TIMESTAMPTZ,
    reply_to UUID REFERENCES direct_messages(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX direct_messages_thread_time_idx ON direct_messages (thread_id,created_at DESC,id DESC);

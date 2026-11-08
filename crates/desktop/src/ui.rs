use std::{collections::HashMap, sync::mpsc::TryRecvError, time::{Duration, Instant}};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use uuid::Uuid;
use utachat_shared::{AuthResponse, Channel, ChatMessage, DirectThread, Friend, GatewayMessage, Member, PublicUser, Server, UserProfile, SearchHit, ChannelUnread, Notification};
use crate::network::{self, Command, Network, UiEvent};
use crate::resources::theme_color;


#[derive(Clone, Copy, PartialEq, Eq)]
enum Modal {
    None,
    CreateServer,
    CreateChannel,
    JoinServer,
    Invite,
    CreateDirect,
    AddFriend,
    EditMessage,
    DeleteMessage,
    Search,
    Pinned,
    Inbox,
    Profile,
    MemberActions,
    RenameServer,
    RenameChannel,
    KickMember,
    BanMember,
}

pub struct UTAchat {
    net: Network,
    user: Option<PublicUser>,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
    directs: Vec<DirectThread>,
    friends: Vec<Friend>,
    profile: Option<UserProfile>,
    search_results: Vec<SearchHit>,
    pinned_messages: Vec<ChatMessage>,
    notifications: Vec<Notification>,
    unreads: HashMap<Uuid, i64>,
    profile_status: String,
    selected_member: Option<Member>,
    direct_mode: bool,
    show_members: bool,
    action_message: Option<Uuid>,
    hovered_message: Option<Uuid>,
    hovered_server: Option<Uuid>,
    hovered_channel: Option<Uuid>,
    reply_to: Option<Uuid>,
    editing_message: Option<Uuid>,
    pending_delete: Option<(Uuid, Uuid, bool)>,
    messages: Vec<ChatMessage>,
    chat_scroll: ScrollHandle,
    selected_server: Option<Uuid>,
    selected_channel: Option<Uuid>,
    pending_channel: Option<Uuid>,
    connected: bool,
    register: bool,
    busy: bool,
    sending: bool,
    pending_message: Option<String>,
    has_older: bool,
    modal: Modal,
    modal_close_at: Option<(Modal, Instant)>,
    invite_code: String,
    error: Option<String>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    composer: Entity<InputState>,
    server_name: Entity<InputState>,
    channel_name: Entity<InputState>,
    invite_input: Entity<InputState>,
    person_input: Entity<InputState>,
    edit_input: Entity<InputState>,
    search_input: Entity<InputState>,
    bio_input: Entity<InputState>,
    rename_input: Entity<InputState>,
    ban_reason: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl UTAchat {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Write a message"));
        let server_name = cx.new(|cx| InputState::new(window, cx).placeholder("Server name"));
        let channel_name = cx.new(|cx| InputState::new(window, cx).placeholder("Channel name"));
        let invite_input = cx.new(|cx| InputState::new(window, cx).placeholder("Invite code"));
        let person_input = cx.new(|cx| InputState::new(window, cx).placeholder("Username"));
        let edit_input = cx.new(|cx| InputState::new(window, cx).placeholder("Edit message"));
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search messages"));
        let bio_input = cx.new(|cx| InputState::new(window, cx).placeholder("About me"));
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        let ban_reason = cx.new(|cx| InputState::new(window, cx).placeholder("Reason"));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send_message(window, cx);
                }
            }),
            cx.subscribe_in(&password, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_auth(cx);
                }
            }),
        ];
        let this = Self {
            net: network::start(), user: None,
            servers: Vec::new(), channels: Vec::new(), members: Vec::new(), messages: Vec::new(),
            directs: Vec::new(), friends: Vec::new(), direct_mode: true,
            profile: None, search_results: Vec::new(), pinned_messages: Vec::new(),
            notifications: Vec::new(), unreads: HashMap::new(),
            profile_status: "online".to_owned(), selected_member: None,
            show_members: true, action_message: None,
            hovered_message: None, hovered_server: None, hovered_channel: None,
            reply_to: None, editing_message: None, pending_delete: None,
            chat_scroll: ScrollHandle::new(),
            selected_server: None, selected_channel: None, pending_channel: None,
            connected: false, register: false, busy: false, sending: false,
            pending_message: None, has_older: false, modal: Modal::None,
            modal_close_at: None,
            invite_code: String::new(), error: None,
            username, password, composer, server_name, channel_name, invite_input,
            person_input, edit_input, search_input, bio_input, rename_input, ban_reason,
            _subscriptions: subscriptions,
        };
        cx.spawn_in(window, async move |weak, window_cx| {
            loop {
                smol::Timer::after(Duration::from_millis(70)).await;
                let result = window_cx.update(|window, cx| {
                    weak.update(cx, |this, cx| this.drain_events(window, cx))
                });
                if !matches!(result, Ok(Ok(()))) {
                    break;
                }
            }
        }).detach();
        this
    }

    fn dismiss_modal(&mut self, cx: &mut Context<Self>) {
        if self.modal != Modal::None && self.modal_close_at.is_none() {
            self.modal_close_at = Some((self.modal, Instant::now() + Duration::from_millis(170)));
            cx.notify();
        }
    }

    fn issue(&mut self, command: Command) {
        if self.net.commands.send(command).is_err() {
            self.error = Some("Network worker stopped".into());
            self.busy = false;
        }
    }

    fn submit_auth(&mut self, cx: &mut Context<Self>) {
        if self.busy { return; }
        let username = self.username.read(cx).value().to_string();
        let password = self.password.read(cx).value().to_string();
        if username.trim().is_empty() || password.is_empty() {
            self.error = Some("Enter your username and password".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        if self.register {
            self.issue(Command::Register { username, password });
        } else {
            self.issue(Command::Login { username, password });
        }
        cx.notify();
    }

    fn send_message(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.sending { return; }
        let Some(channel) = self.selected_channel else { return; };
        let content = self.composer.read(cx).value().to_string();
        if content.trim().is_empty() { return; }
        self.sending = true;
        self.pending_message = Some(content.clone());
        self.error = None;
        let reply_to = self.reply_to;
        if self.direct_mode {
            self.issue(Command::SendDirect { thread: channel, content, reply_to });
        } else {
            self.issue(Command::SendMessage { channel, content, reply_to });
        }
        cx.notify();
    }

    fn select_server(&mut self, server: Uuid, cx: &mut Context<Self>) {
        if self.selected_server == Some(server) && !self.direct_mode { return; }
        self.direct_mode = false;
        self.action_message = None;
        self.hovered_message = None;
        self.reply_to = None;
        self.selected_server = Some(server);
        self.selected_channel = None;
        self.channels.clear();
        self.members.clear();
        self.messages.clear();
        self.has_older = false;
        self.issue(Command::SelectServer(server));
        cx.notify();
    }

    fn open_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bio = self.profile.as_ref().map(|profile| profile.bio.clone()).unwrap_or_default();
        self.profile_status = self.profile.as_ref().map(|profile| profile.status.clone())
            .unwrap_or_else(|| "online".to_owned());
        self.bio_input.update(cx, |input, cx| input.set_value(bio, window, cx));
        self.modal = Modal::Profile;
        self.error = None;
        cx.notify();
    }

    fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.selected_server.is_none() || self.direct_mode { return; }
        self.search_results.clear();
        self.modal = Modal::Search;
        self.error = None;
        cx.notify();
    }

    fn open_pins(&mut self, cx: &mut Context<Self>) {
        if self.direct_mode { return; }
        if let Some(channel) = self.selected_channel {
            self.pinned_messages.clear();
            self.modal = Modal::Pinned;
            self.issue(Command::FetchPins(channel));
            cx.notify();
        }
    }

    fn open_inbox(&mut self, cx: &mut Context<Self>) {
        self.modal = Modal::Inbox;
        self.issue(Command::FetchNotifications);
        cx.notify();
    }

    fn select_channel(&mut self, channel: Uuid, cx: &mut Context<Self>) {
        if self.selected_channel == Some(channel) { return; }
        self.selected_channel = Some(channel);
        self.hovered_message = None;
        self.action_message = None;
        self.reply_to = None;
        self.messages.clear();
        self.chat_scroll = ScrollHandle::new();
        self.has_older = false;
        self.issue(Command::SelectChannel(channel));
        cx.notify();
    }

    fn enter_direct(&mut self, cx: &mut Context<Self>) {
        self.direct_mode = true;
        self.action_message = None;
        self.hovered_message = None;
        self.selected_channel = None;
        self.reply_to = None;
        self.messages.clear();
        self.issue(Command::FetchDirects);
        self.issue(Command::FetchFriends);
        cx.notify();
    }

    fn select_direct(&mut self, thread: Uuid, cx: &mut Context<Self>) {
        if self.direct_mode && self.selected_channel == Some(thread) { return; }
        self.direct_mode = true;
        self.action_message = None;
        self.hovered_message = None;
        self.selected_channel = Some(thread);
        self.reply_to = None;
        self.chat_scroll = ScrollHandle::new();
        self.messages.clear();
        self.has_older = false;
        self.issue(Command::SelectDirect(thread));
        cx.notify();
    }

    fn update_message(&mut self, message: ChatMessage) {
        if let Some(existing) = self.messages.iter_mut().find(|existing| existing.id == message.id) {
            *existing = message;
        } else {
            self.push_message(message);
        }
    }

    fn remove_message(&mut self, channel: Uuid, message: Uuid) {
        if self.selected_channel == Some(channel) {
            self.messages.retain(|existing| existing.id != message);
            if self.reply_to == Some(message) { self.reply_to = None; }
        }
    }

    fn push_message(&mut self, message: ChatMessage) {
        if Some(message.channel_id) == self.selected_channel && !self.messages.iter().any(|m| m.id == message.id) {
            self.messages.push(message);
            self.messages.sort_by_key(|m| (m.created_at, m.id));
        }
    }

    fn drain_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        loop {
            let event = match self.net.events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.error.as_deref() != Some("Network worker stopped") {
                        self.error = Some("Network worker stopped".into());
                        changed = true;
                    }
                    break;
                }
            };
            changed = true;
            match event {
                UiEvent::Authenticated(AuthResponse { user, .. }) => {
                    self.issue(Command::FetchNotifications);
                    self.user = Some(user);
                    self.busy = false;
                    self.error = None;
                    self.modal = Modal::None;
                    self.password.update(cx, |input, cx| input.set_value("", window, cx));
                }
                UiEvent::LoggedOut => {
                    self.user = None;
                    self.servers.clear();
                    self.channels.clear();
                    self.directs.clear();
                    self.friends.clear();
                    self.profile = None;
                    self.unreads.clear();
                    self.notifications.clear();
                    self.search_results.clear();
                    self.direct_mode = true;
                    self.reply_to = None;
                    self.members.clear();
                    self.messages.clear();
                    self.selected_server = None;
                    self.selected_channel = None;
                    self.pending_channel = None;
                    self.connected = false;
                    self.busy = false;
                    self.modal = Modal::None;
                    self.error = None;
                }
                UiEvent::Servers(servers) => {
                    self.servers = servers;
                    if !self.direct_mode && self.selected_server.is_none_or(|id| !self.servers.iter().any(|s| s.id == id)) {
                        if let Some(server) = self.servers.first().map(|s| s.id) {
                            self.select_server(server, cx);
                        } else {
                            self.selected_server = None;
                        }
                    }
                }
                UiEvent::Channels { server, items } if Some(server) == self.selected_server => {
                    self.channels = items;
                    if self.selected_channel.is_none_or(|id| !self.channels.iter().any(|c| c.id == id)) {
                        let target = self.pending_channel.take().filter(|id| self.channels.iter().any(|c| c.id == *id))
                            .or_else(|| self.channels.first().map(|c| c.id));
                        if let Some(channel) = target {
                            self.select_channel(channel, cx);
                        } else {
                            self.selected_channel = None;
                            self.messages.clear();
                        }
                    }
                }
                UiEvent::Members { server, items } if Some(server) == self.selected_server => {
                    self.members = items;
                }
                UiEvent::Messages { channel, items } if Some(channel) == self.selected_channel => {
                    self.has_older = items.len() == 100;
                    self.messages = items;
                    self.chat_scroll.scroll_to_bottom();
                }
                UiEvent::OlderMessages { channel, items } if Some(channel) == self.selected_channel => {
                    self.has_older = items.len() == 100;
                    for message in items {
                        self.push_message(message);
                    }
                }
                UiEvent::MessagePosted(message) => {
                    self.push_message(message);
                    self.reply_to = None;
                    self.chat_scroll.scroll_to_bottom();
                    self.sending = false;
                    if let Some(pending) = self.pending_message.take() {
                        if self.composer.read(cx).value().to_string() == pending {
                            self.composer.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                }
                UiEvent::CreatedServer(server) | UiEvent::JoinedServer(server) => {
                    self.modal = Modal::None;
                    self.busy = false;
                    self.selected_server = None;
                    self.select_server(server.id, cx);
                }
                UiEvent::CreatedChannel(channel) => {
                    self.busy = false;
                    self.modal = Modal::None;
                    if Some(channel.server_id) == self.selected_server {
                        self.channels.retain(|c| c.id != channel.id);
                        self.channels.push(channel.clone());
                        self.channels.sort_by_key(|c| c.position);
                        self.select_channel(channel.id, cx);
                    }
                }
                UiEvent::InviteCode(code) => {
                    self.invite_code = code;
                    self.busy = false;
                    self.modal = Modal::Invite;
                }
                UiEvent::Profile(profile) => {
                    self.profile = Some(profile);
                }
                UiEvent::ProfileSaved(profile) => {
                    self.profile_status = profile.status.clone();
                    self.profile = Some(profile);
                    self.busy = false;
                    self.modal = Modal::None;
                }
                UiEvent::SearchResults(hits) => {
                    self.search_results = hits;
                    self.busy = false;
                }
                UiEvent::PinnedMessages(items) => {
                    self.pinned_messages = items;
                    self.busy = false;
                }
                UiEvent::Notifications(items) => {
                    self.notifications = items;
                    self.busy = false;
                }
                UiEvent::Unreads(items) => {
                    self.unreads = items.into_iter().map(|item| (item.channel_id, item.count)).collect();
                }
                UiEvent::RoleChanged | UiEvent::MemberKicked(_) | UiEvent::MemberBanned(_) => {
                    self.busy = false;
                    self.modal = Modal::None;
                    if let Some(server) = self.selected_server {
                        self.issue(Command::SelectServer(server));
                    }
                }
                UiEvent::ServerRenamed(server) => {
                    if let Some(item) = self.servers.iter_mut().find(|item| item.id == server.id) { *item = server; }
                    self.busy = false;
                    self.modal = Modal::None;
                }
                UiEvent::ChannelRenamed(channel) => {
                    if let Some(item) = self.channels.iter_mut().find(|item| item.id == channel.id) { *item = channel; }
                    self.busy = false;
                    self.modal = Modal::None;
                }
                UiEvent::LocatedChannel(channel) => {
                    self.pending_channel = Some(channel.id);
                    if self.selected_server != Some(channel.server_id) || self.direct_mode {
                        self.select_server(channel.server_id, cx);
                    } else {
                        self.pending_channel = None;
                        if self.selected_channel == Some(channel.id) {
                            self.issue(Command::SelectChannel(channel.id));
                        } else {
                            self.select_channel(channel.id, cx);
                        }
                    }
                }
                UiEvent::DirectThreads(items) => {
                    self.directs = items;
                }
                UiEvent::Friends(items) => {
                    self.friends = items;
                    self.busy = false;
                    if self.modal == Modal::AddFriend { self.modal = Modal::None; }
                }
                UiEvent::DirectCreated(thread) => {
                    self.directs.retain(|entry| entry.id != thread.id);
                    self.directs.insert(0, thread.clone());
                    self.modal = Modal::None;
                    self.busy = false;
                    self.select_direct(thread.id, cx);
                }
                UiEvent::MessageUpdated(message) => {
                    self.update_message(message);
                    self.modal = Modal::None;
                    self.busy = false;
                    self.editing_message = None;
                }
                UiEvent::MessageDeleted { channel, message } => {
                    self.remove_message(channel, message);
                    self.modal = Modal::None;
                    self.pending_delete = None;
                    self.action_message = None;
                    self.busy = false;
                },
                UiEvent::MessageReactions(channel) => {
                    if !self.direct_mode && self.selected_channel == Some(channel) {
                        self.issue(Command::SelectChannel(channel));
                    }
                }
                UiEvent::Connection(online) => {
                    let was_online = self.connected;
                    self.connected = online;
                    if online && !was_online {
                        if let Some(channel) = self.selected_channel {
                            if self.direct_mode {
                                self.issue(Command::SelectDirect(channel));
                            } else {
                                self.issue(Command::SelectChannel(channel));
                            }
                        }
                    }
                }
                UiEvent::Gateway(GatewayMessage::MessageCreated { message }) => {
                    if !self.direct_mode && self.selected_channel == Some(message.channel_id) {
                        self.issue(Command::MarkRead(message.channel_id));
                    } else {
                        self.issue(Command::FetchUnreads);
                    }
                    self.push_message(message);
                }
                UiEvent::Gateway(GatewayMessage::DirectMessageCreated { message }) => self.push_message(message),
                UiEvent::Gateway(GatewayMessage::MessageUpdated { message })
                | UiEvent::Gateway(GatewayMessage::DirectMessageUpdated { message }) => self.update_message(message),
                UiEvent::Gateway(GatewayMessage::MessageDeleted { channel_id, message_id })
                | UiEvent::Gateway(GatewayMessage::DirectMessageDeleted { channel_id, message_id }) => {
                    self.remove_message(channel_id, message_id);
                    self.action_message = None;
                    if self.pending_delete.is_some_and(|(_, pending, _)| pending == message_id) {
                        self.pending_delete = None;
                        self.modal = Modal::None;
                        self.busy = false;
                    }
                },
                UiEvent::Gateway(GatewayMessage::ReactionsChanged { channel_id }) => {
                    if !self.direct_mode && self.selected_channel == Some(channel_id) {
                        self.issue(Command::SelectChannel(channel_id));
                    }
                }
                UiEvent::Gateway(GatewayMessage::DirectThreadCreated { thread }) => {
                    if !self.directs.iter().any(|item| item.id == thread.id) {
                        self.directs.insert(0, thread);
                    }
                }
                UiEvent::Gateway(GatewayMessage::FriendsChanged) => self.issue(Command::FetchFriends),
                UiEvent::Gateway(GatewayMessage::NotificationCreated { notification }) => {
                    if !self.notifications.iter().any(|item| item.id == notification.id) {
                        self.notifications.insert(0, notification);
                    }
                    self.issue(Command::FetchUnreads);
                }
                UiEvent::Gateway(GatewayMessage::ProfileUpdated { profile }) => {
                    self.profile = Some(profile);
                }
                UiEvent::Gateway(GatewayMessage::PinsChanged { channel_id }) => {
                    if self.modal == Modal::Pinned && self.selected_channel == Some(channel_id) {
                        self.issue(Command::FetchPins(channel_id));
                    }
                    if self.selected_channel == Some(channel_id) {
                        self.issue(Command::SelectChannel(channel_id));
                    }
                }
                UiEvent::Gateway(GatewayMessage::ChannelDeleted { server_id, channel_id }) => {
                    if self.selected_server == Some(server_id) {
                        self.channels.retain(|channel| channel.id != channel_id);
                        if self.selected_channel == Some(channel_id) {
                            self.selected_channel = None;
                            if let Some(next) = self.channels.first().map(|channel| channel.id) {
                                self.select_channel(next, cx);
                            }
                        }
                    }
                }
                UiEvent::Gateway(GatewayMessage::MemberRemoved { server_id, user_id }) => {
                    if self.selected_server == Some(server_id) {
                        if self.user.as_ref().is_some_and(|user| user.id == user_id) {
                            self.selected_server = None;
                            self.selected_channel = None;
                            self.channels.clear();
                            self.members.clear();
                            self.messages.clear();
                            self.direct_mode = true;
                            self.issue(Command::FetchServers);
                        } else {
                            self.issue(Command::SelectServer(server_id));
                        }
                    }
                }
                UiEvent::Gateway(GatewayMessage::MemberRoleChanged { server_id, .. }) => {
                    if self.selected_server == Some(server_id) {
                        self.issue(Command::SelectServer(server_id));
                    }
                },
                UiEvent::Gateway(GatewayMessage::ChannelCreated { channel }) => {
                    if Some(channel.server_id) == self.selected_server {
                        if let Some(existing) = self.channels.iter_mut().find(|c| c.id == channel.id) {
                            *existing = channel;
                        } else {
                            self.channels.push(channel);
                        }
                        self.channels.sort_by_key(|c| c.position);
                    }
                }
                UiEvent::Gateway(GatewayMessage::MemberJoined { server_id, .. }) => {
                    if Some(server_id) == self.selected_server {
                        self.issue(Command::SelectServer(server_id));
                    }
                }
                UiEvent::Gateway(GatewayMessage::Error { message }) => {
                    self.error = Some(message);
                    if let Some(channel) = self.selected_channel {
                        if self.direct_mode { self.issue(Command::SelectDirect(channel)); }
                        else { self.issue(Command::SelectChannel(channel)); }
                    }
                }
                UiEvent::Error(message) => {
                    self.error = Some(message);
                    self.busy = false;
                    self.sending = false;
                    self.pending_message = None;
                }
                _ => {}
            }
        }
        if let Some((closing, end)) = self.modal_close_at {
            if self.modal != closing || Instant::now() >= end {
                if self.modal == closing {
                    self.modal = Modal::None;
                    self.error = None;
                }
                self.modal_close_at = None;
                changed = true;
            }
        }
        if changed { cx.notify(); }
    }

    fn render_auth(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = if self.register { "Create account" } else { "Sign in" };
        div()
            .size_full().bg(rgb(theme_color("content"))).font_family("Segoe UI").text_color(rgb(theme_color("text"))).flex().items_center().justify_center()
            .child(
                div().w(px(420.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border"))).p_8().flex().flex_col().gap_5()
                    .child(div().text_size(px(27.)).font_weight(FontWeight::BOLD).child("UTAchat"))
                    .child(div().text_size(px(13.)).text_color(rgb(theme_color("muted"))).child(if self.register { "Create your UTAchat account" } else { "Sign in to UTAchat" }))
                    .child(div().flex().flex_col().gap_2()
                        .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted"))).child("USERNAME"))
                        .child(input_frame(&self.username)))
                    .child(div().flex().flex_col().gap_2()
                        .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted"))).child("PASSWORD"))
                        .child(input_frame(&self.password)))
                    .when_some(self.error.as_ref(), |el, error| el.child(div().text_sm().text_color(rgb(theme_color("danger_text"))).child(error.clone())))
                    .child(div().id("submit-auth").h(px(44.)).flex().items_center().justify_center()
                        .rounded_md().bg(rgb(theme_color("accent"))).cursor_pointer().font_weight(FontWeight::SEMIBOLD)
                        .child(if self.busy { "Connecting" } else { label })
                        .on_click(cx.listener(|this, _, _, cx| this.submit_auth(cx))))
                    .child(div().id("toggle-auth").text_sm().text_color(rgb(theme_color("auth_link"))).cursor_pointer()
                        .child(if self.register { "Already have an account? Sign in" } else { "Need an account? Register" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.register = !this.register;
                            this.error = None;
                            cx.notify();
                        })))
            ).into_any_element()
    }

    fn render_server_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rail = div().id("server-rail").w(px(72.)).h_full().min_h_0().overflow_y_scrollbar().bg(rgb(theme_color("rail"))).py_3().flex().flex_col().items_center().gap_2()
            .child(div().id("home-direct").size(px(46.))
                .when(self.direct_mode, |el| el.rounded_lg())
                .when(!self.direct_mode, |el| el.rounded_full())
                .bg(rgb(if self.direct_mode { theme_color("accent") } else { theme_color("server_inactive") }))
                .flex().items_center().justify_center().cursor_pointer()
                .font_weight(FontWeight::BOLD).text_base().child(svg().path("icons/utachat-mark.svg").size(px(30.)).text_color(rgb(theme_color("text"))))
                .on_click(cx.listener(|this, _, _, cx| this.enter_direct(cx))))
            .child(div().h(px(2.)).w(px(34.)).bg(rgb(theme_color("line"))));
        for server in &self.servers {
            let id = server.id;
            let name = server.name.chars().take(2).collect::<String>().to_uppercase();
            let active = !self.direct_mode && Some(id) == self.selected_server;
            let hovered = self.hovered_server == Some(id);
            let target = if active { theme_color("accent") } else if hovered {
                theme_color("sidebar_hover")
            } else {
                theme_color("server_inactive")
            };
            rail = rail.child(
                div().relative().w(px(60.)).h(px(48.)).flex().items_center().justify_center()
                    .when(active, |el| el.child(div().absolute().left(px(0.)).w(px(4.)).h(px(36.))
                        .rounded_full().bg(rgb(theme_color("text")))
                        .with_animation(format!("server-marker-{id}"), entrance(180),
                            |el, t| el.opacity(t).h(px(18. + 18. * t)))))
                    .child(div().id(format!("server-{id}")).size(px(46.))
                        .when(active || hovered, |el| el.rounded_lg())
                        .when(!active && !hovered, |el| el.rounded_full())
                        .flex().items_center().justify_center()
                        .bg(rgb(target))
                        .text_color(rgb(theme_color("text"))).font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .cursor_pointer().child(name)
                        .on_hover(cx.listener(move |this, &inside, _, cx| {
                            if inside {
                                this.hovered_server = Some(id);
                            } else if this.hovered_server == Some(id) {
                                this.hovered_server = None;
                            }
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_server(id, cx)))
                        .with_animation(format!("server-state-{id}-{active}-{hovered}"), entrance(160),
                            move |el, t| el.bg(rgb(blend_color(
                                if active { theme_color("sidebar_hover") }
                                else if hovered { theme_color("server_inactive") }
                                else { theme_color("sidebar_hover") },
                                target,
                                t
                            )))))
            );
        }
        rail.child(div().id("create-server-trigger").size(px(44.)).rounded_full().bg(rgb(theme_color("server_action_bg")))
            .flex().items_center().justify_center().text_color(rgb(theme_color("server_add"))).text_xl().cursor_pointer()
            .child("+").on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateServer; this.error = None; cx.notify(); })))
            .child(div().id("join-server-trigger").size(px(44.)).rounded_full().bg(rgb(theme_color("server_action_bg")))
            .flex().items_center().justify_center().text_color(rgb(theme_color("muted"))).text_sm().cursor_pointer()
            .child(svg().path("icons/compass.svg").size(px(23.)).text_color(rgb(theme_color("muted"))))
            .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::JoinServer; this.error = None; cx.notify(); })))
            .into_any_element()
    }

    fn render_user_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let user = self.user.as_ref();
        let username = user.map(|user| user.username.clone()).unwrap_or_default();
        let color = user.map(|user| avatar_color(user.id)).unwrap_or(theme_color("avatar_0"));
        let (status_text, status_color) = if !self.connected {
            ("Connecting", theme_color("reconnecting"))
        } else {
            match self.profile_status.as_str() {
                "idle" => ("Idle", theme_color("status_idle")),
                "dnd" => ("Do Not Disturb", theme_color("status_dnd")),
                "invisible" => ("Invisible", theme_color("status_offline")),
                _ => ("Online", theme_color("online")),
            }
        };
        div().h(px(60.)).w_full().px_3().bg(rgb(theme_color("profile_bar")))
            .flex().items_center().justify_between()
            .child(div().min_w_0().flex().items_center().gap_3()
                .child(div().relative().size(px(36.)).rounded_full()
                    .bg(rgb(color)).flex().items_center().justify_center()
                    .text_size(px(13.)).font_weight(FontWeight::BOLD)
                    .child(initials(&username))
                    .child(div().absolute().right(px(0.)).bottom(px(0.)).size(px(12.))
                        .rounded_full().border_2().border_color(rgb(theme_color("profile_bar")))
                        .bg(rgb(status_color))))
                .child(div().min_w_0().flex().flex_col().gap_1()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD)
                        .child(username))
                    .child(div().text_size(px(11.5)).text_color(rgb(theme_color("muted")))
                        .child(status_text))))
            .child(div().id("settings-trigger").size(px(34.)).rounded_md()
                .cursor_pointer().flex().items_center().justify_center()
                .child(svg().path("icons/settings.svg").size(px(20.))
                    .text_color(rgb(theme_color("muted"))))
                .on_click(cx.listener(|this, _, window, cx| this.open_profile(window, cx))))
            .into_any_element()
    }

    fn render_direct_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().id("direct-scroll").flex_1().min_h_0().overflow_y_scrollbar()
            .p_3().flex().flex_col().gap_2()
            .child(div().id("add-friend").h(px(36.)).px_3().rounded_md()
                .bg(rgb(theme_color("server_action_bg"))).cursor_pointer()
                .flex().items_center().justify_between()
                .child(div().text_sm().text_color(rgb(theme_color("text"))).child("Add Friend"))
                .child(div().text_lg().text_color(rgb(theme_color("muted"))).child("+"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.modal = Modal::AddFriend;
                    this.error = None;
                    cx.notify();
                })))
            .child(div().px_2().pt_2().pb_1().text_xs().font_weight(FontWeight::BOLD)
                .text_color(rgb(theme_color("muted"))).child("DIRECT MESSAGES"));
        for thread in &self.directs {
            let id = thread.id;
            let active = self.selected_channel == Some(id);
            list = list.child(div().id(format!("dm-{id}")).h(px(39.)).px_3().rounded_md()
                .flex().items_center().gap_3().cursor_pointer()
                .bg(rgb(if active { theme_color("selection") } else { theme_color("sidebar") }))
                .child(div().size(px(28.)).rounded_full().bg(rgb(avatar_color(thread.peer.id)))
                    .flex().items_center().justify_center().text_xs().font_weight(FontWeight::BOLD)
                    .child(initials(&thread.peer.username)))
                .child(div().text_sm().child(thread.peer.username.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.select_direct(id, cx))));
        }
        list = list.child(div().px_2().pt_3().pb_1().text_xs().font_weight(FontWeight::BOLD)
            .text_color(rgb(theme_color("muted"))).child("FRIENDS"));
        for friend in &self.friends {
            let id = friend.user.id;
            let name = friend.user.username.clone();
            if friend.status == "accepted" {
                let username = name.clone();
                list = list.child(div().h(px(34.)).px_3().flex().items_center().justify_between()
                    .child(div().text_sm().text_color(rgb(theme_color("text"))).child(name))
                    .child(div().id(format!("message-friend-{id}")).text_xs().text_color(rgb(theme_color("link")))
                        .cursor_pointer().child("MESSAGE")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.busy = true;
                            this.issue(Command::CreateDirect(username.clone()));
                            cx.notify();
                        }))));
            } else if friend.incoming {
                list = list.child(div().h(px(34.)).px_3().flex().items_center().justify_between()
                    .child(div().text_sm().child(name))
                    .child(div().id(format!("accept-friend-{id}")).text_xs().text_color(rgb(theme_color("friend_accept")))
                        .cursor_pointer().child("ACCEPT")
                        .on_click(cx.listener(move |this, _, _, cx| this.issue(Command::AcceptFriend(id))))));
            } else {
                list = list.child(div().h(px(34.)).px_3().flex().items_center().justify_between()
                    .child(div().text_sm().text_color(rgb(theme_color("muted"))).child(name))
                    .child(div().text_xs().text_color(rgb(theme_color("muted"))).child("PENDING")));
            }
        }
        div().w(px(240.)).h_full().flex().flex_col().bg(rgb(theme_color("sidebar")))
            .child(div().h(px(52.)).px_4().border_b_1().border_color(rgb(theme_color("separator")))
                .flex().items_center().justify_between()
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Direct Messages"))
                .child(div().id("new-direct").text_xs().text_color(rgb(theme_color("link"))).cursor_pointer()
                    .flex().items_center().gap_2().child(svg().path("icons/new-message.svg").size(px(16.)).text_color(rgb(theme_color("muted")))).child("NEW")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::CreateDirect;
                        this.error = None;
                        cx.notify();
                    }))))
            .child(list)
            .child(self.render_user_panel(cx))
        .into_any_element()
    }

    fn render_channel_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let server_name = self.selected_server.and_then(|id| self.servers.iter().find(|s| s.id == id))
            .map(|s| s.name.clone()).unwrap_or_else(|| "No server selected".into());
        let can_create = self.user.as_ref().is_some_and(|user|
            self.members.iter().any(|member| member.user.id == user.id &&
                matches!(member.role.as_str(), "owner" | "moderator")));
        let mut channel_list = div().id("channel-scroll").flex_1().min_h_0().overflow_y_scrollbar().px_2().py_4().flex().flex_col().gap_1()
            .child(div().h(px(30.)).px_2().flex().items_center().justify_between()
                .child(div().text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted"))).child("TEXT CHANNELS"))
                .when(can_create, |header| header.child(div().id("create-channel-trigger").cursor_pointer()
                    .text_lg().text_color(rgb(theme_color("muted"))).child("+")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateChannel; this.error = None; cx.notify(); })))));
        for channel in &self.channels {
            let id = channel.id;
            let active = Some(id) == self.selected_channel;
            let unread = self.unreads.get(&id).copied().unwrap_or(0);
            let hovered = self.hovered_channel == Some(id);
            let background = if active { theme_color("selection") } else if hovered {
                theme_color("sidebar_hover")
            } else {
                theme_color("sidebar")
            };
            channel_list = channel_list.child(
                div().id(format!("channel-{id}")).h(px(36.)).px_3().rounded_md()
                    .flex().items_center().gap_2().cursor_pointer()
                    .bg(rgb(background))
                    .text_color(rgb(if active || unread > 0 { theme_color("text") } else { theme_color("muted") }))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::NORMAL)
                        .text_color(rgb(theme_color("subtle"))).child("#"))
                    .child(div().text_size(px(14.)).font_weight(if active || unread > 0 {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    }).child(channel.name.clone()))
                    .when(unread > 0, |el| el.child(div().ml_auto().px_2().py_1().rounded_lg()
                        .bg(rgb(theme_color("danger_text"))).text_xs()
                        .text_color(rgb(theme_color("rail"))).font_weight(FontWeight::BOLD)
                        .child(if unread > 99 { "99+".to_owned() } else { unread.to_string() })))
                    .on_hover(cx.listener(move |this, &inside, _, cx| {
                        if inside {
                            this.hovered_channel = Some(id);
                        } else if this.hovered_channel == Some(id) {
                            this.hovered_channel = None;
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_channel(id, cx)))
                    .with_animation(format!("channel-motion-{id}-{active}-{hovered}"),
                        entrance(155), move |el, t| el.bg(rgb(blend_color(
                            if active { theme_color("sidebar_hover") }
                            else if hovered { theme_color("sidebar") }
                            else { theme_color("sidebar_hover") },
                            background,
                            t
                        ))))
            );
        }
        div().w(px(240.)).h_full().flex().flex_col().bg(rgb(theme_color("sidebar")))
            .child(div().h(px(52.)).px_4().border_b_1().border_color(rgb(theme_color("separator")))
                .flex().items_center().justify_between()
                .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child(server_name))
                .child(div().id("invite-trigger").cursor_pointer().flex().items_center().gap_2()
                    .text_size(px(13.)).text_color(rgb(theme_color("muted")))
                    .child(svg().path("icons/user-plus.svg").size(px(17.))
                        .text_color(rgb(theme_color("muted"))))
                    .child("Invite").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(server) = this.selected_server {
                            this.busy = true;
                            this.issue(Command::FetchInvite(server));
                        }
                        cx.notify();
                    })))
                .when(can_create, |el| el.child(div().id("rename-server-trigger")
                    .size(px(26.)).rounded_md().cursor_pointer()
                    .flex().items_center().justify_center()
                    .child(svg().path("icons/settings.svg").size(px(16.))
                        .text_color(rgb(theme_color("muted"))))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let name = this.selected_server.and_then(|id|
                            this.servers.iter().find(|server| server.id == id))
                            .map(|server| server.name.clone()).unwrap_or_default();
                        this.rename_input.update(cx, |input, cx| input.set_value(name, window, cx));
                        this.modal = Modal::RenameServer;
                        cx.notify();
                    })))))
            .child(channel_list)
            .child(self.render_user_panel(cx))
            .into_any_element()
    }

    fn render_conversation_intro(&self, channel_name: &str) -> AnyElement {
        let heading = if self.direct_mode {
            channel_name.to_owned()
        } else {
            format!("Welcome to #{}", channel_name)
        };
        let description = if self.direct_mode {
            format!("The beginning of your conversation with {}.", channel_name)
        } else {
            format!("This is the beginning of #{}.", channel_name)
        };
        div().w_full().px_6().flex().flex_col().items_start().gap_3()
            .child(div().size(px(64.)).rounded_full().bg(rgb(theme_color("selection")))
                .flex().items_center().justify_center()
                .text_size(px(31.)).font_weight(FontWeight::BOLD)
                .child(if self.direct_mode { "@" } else { "#" }))
            .child(div().text_size(px(27.)).font_weight(FontWeight::BOLD).child(heading))
            .child(div().text_size(px(14.)).text_color(rgb(theme_color("muted")))
                .child(description))
            .child(div().w_full().h(px(1.)).mt_4().mb_2().bg(rgb(theme_color("line"))))
            .into_any_element()
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let current_channel = self.selected_channel.and_then(|id| self.channels.iter().find(|c| c.id == id));
        let channel_name = if self.direct_mode {
            self.selected_channel.and_then(|id| self.directs.iter().find(|thread| thread.id == id))
                .map(|thread| thread.peer.username.as_str()).unwrap_or("Direct messages")
        } else {
            current_channel.map(|c| c.name.as_str()).unwrap_or("Select a channel")
        };
        let can_pin = !self.direct_mode && self.user.as_ref().is_some_and(|user|
            self.members.iter().any(|member| member.user.id == user.id &&
                matches!(member.role.as_str(), "owner" | "moderator")));
        let mut stream = div().id("message-scroll").flex_1().min_h_0()
            .track_scroll(&self.chat_scroll).overflow_y_scrollbar()
            .py_4().flex().flex_col().gap_1()
            .child(div().flex_1().min_h(px(8.)));
        if self.has_older && self.selected_channel.is_some() {
            stream = stream.child(div().id("older-trigger").h(px(32.)).mb_4().flex().items_center().justify_center()
                .text_xs().text_color(rgb(theme_color("link"))).cursor_pointer().child("Load earlier messages")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let (Some(channel), Some(oldest)) = (this.selected_channel, this.messages.first()) {
                        if this.direct_mode {
                            this.issue(Command::LoadOlderDirect { thread: channel, before: oldest.id });
                        } else {
                            this.issue(Command::LoadOlder { channel, before: oldest.id });
                        }
                    }
                    cx.notify();
                })));
        }
        if self.selected_channel.is_some() && !self.has_older {
            stream = stream.child(self.render_conversation_intro(channel_name));
        }
        for (i, message) in self.messages.iter().enumerate() {
            let new_day = i == 0 || self.messages[i - 1].created_at.date_naive()
                != message.created_at.date_naive();
            if new_day {
                stream = stream.child(div().w_full().px_5().py_3().flex().items_center().gap_3()
                    .child(div().flex_1().h(px(1.)).bg(rgb(theme_color("line"))))
                    .child(div().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme_color("muted")))
                        .child(message.created_at.format("%B %-d, %Y").to_string()))
                    .child(div().flex_1().h(px(1.)).bg(rgb(theme_color("line")))));
            }
            let continuation = i > 0 && !new_day && self.messages[i - 1].author.id == message.author.id
                && (message.created_at - self.messages[i - 1].created_at).num_minutes() < 5;
            let expanded = self.action_message == Some(message.id);
            let hovered = self.hovered_message == Some(message.id);
            stream = stream.child(render_message(message, continuation,
                self.user.as_ref().map(|u| u.id), self.direct_mode, expanded, hovered, can_pin, cx));
        }
        let mut center = div().flex_1().min_w_0().h_full().flex().flex_col().bg(rgb(theme_color("content")))
            .child(div().h(px(52.)).bg(rgb(theme_color("header"))).border_b_1().border_color(rgb(theme_color("header_separator"))).px_5()
                .flex().items_center().justify_between()
                .child(div().flex().items_center().gap_3()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted")))
                        .child(if self.direct_mode { "@" } else { "#" }))
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(channel_name.to_string())))
                .child(div().flex().items_center().gap_2()
                    .when(!self.direct_mode, |el| el.child(div().id("search-trigger")
                        .size(px(36.)).rounded_md().cursor_pointer()
                        .flex().items_center().justify_center()
                        .child(svg().path("icons/search.svg").size(px(20.))
                            .text_color(rgb(theme_color("muted"))))
                        .on_click(cx.listener(|this, _, _, cx| this.open_search(cx)))))
                    .when(!self.direct_mode, |el| el.child(div().id("pins-trigger")
                        .size(px(36.)).rounded_md().cursor_pointer()
                        .flex().items_center().justify_center()
                        .child(svg().path("icons/pin.svg").size(px(20.))
                            .text_color(rgb(theme_color("muted"))))
                        .on_click(cx.listener(|this, _, _, cx| this.open_pins(cx)))))
                    .child(div().id("inbox-trigger").size(px(36.)).rounded_md().cursor_pointer()
                        .flex().items_center().justify_center()
                        .child(svg().path("icons/inbox.svg").size(px(20.))
                            .text_color(rgb(theme_color("muted"))))
                        .on_click(cx.listener(|this, _, _, cx| this.open_inbox(cx))))
                    .when(!self.direct_mode, |el| el.child(div().id("members-toggle").size(px(36.))
                        .cursor_pointer().rounded_md().flex().items_center().justify_center()
                        .bg(rgb(if self.show_members { theme_color("selection") } else { theme_color("header") }))
                        .child(svg().path("icons/users.svg").size(px(20.))
                            .text_color(rgb(if self.show_members { theme_color("text") } else { theme_color("muted") })))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_members = !this.show_members;
                            cx.notify();
                        }))))
                    .when(!self.connected, |el| el.child(div().text_xs()
                        .text_color(rgb(theme_color("reconnecting"))).child("Reconnecting")))))
            .child(stream);
        if self.selected_channel.is_some() {
            center = center.child(div().px_5().pb_5().pt_2()
                .when_some(self.reply_to, |el, id| el.child(div().h(px(28.)).flex().items_center().gap_3()
                    .child(div().text_xs().text_color(rgb(theme_color("muted"))).child(format!("Reply to {}", &id.to_string()[..8])))
                    .child(div().id("cancel-reply").text_xs().cursor_pointer().text_color(rgb(theme_color("link")))
                        .child("CANCEL").on_click(cx.listener(|this, _, _, cx| {
                            this.reply_to = None;
                            cx.notify();
                        })))))
                .child(div().h(px(52.)).bg(rgb(theme_color("input_bg"))).rounded_md()
                    .pl_4().pr_2().flex().items_center().gap_3()
                    .child(div().flex_1().min_w_0().text_size(px(15.))
                        .child(Input::new(&self.composer).appearance(false).bordered(false).w_full()))
                    .child(div().id("send-trigger").size(px(36.)).rounded_md()
                        .bg(rgb(theme_color("input_bg")))
                        .cursor_pointer().flex().items_center().justify_center()
                        .child(svg().path("icons/send.svg").size(px(18.)).text_color(rgb(theme_color("muted"))))
                        .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx))))));
        }
        center.into_any_element()
    }

    fn render_members(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().id("member-scroll").w(px(240.)).h_full().min_h_0()
            .overflow_y_scrollbar().bg(rgb(theme_color("member_bg")))
            .p_3().flex().flex_col().gap_1();
        for (role, title) in [("owner", "SERVER OWNER"), ("moderator", "MODERATORS"), ("member", "MEMBERS")] {
            let group: Vec<_> = self.members.iter().filter(|member| member.role == role).collect();
            if group.is_empty() { continue; }
            body = body.child(div().h(px(34.)).px_2().pt_2()
                .flex().items_center().text_xs().font_weight(FontWeight::BOLD)
                .text_color(rgb(theme_color("muted")))
                .child(format!("{} — {}", title, group.len())));
            for member in group {
                let username = member.user.username.clone();
                let avatar = initials(&username);
                let color = avatar_color(member.user.id);
                let target = member.clone();
                let can_manage = self.user.as_ref().is_some_and(|user|
                    self.members.iter().any(|item| item.user.id == user.id &&
                        matches!(item.role.as_str(), "owner" | "moderator"))) &&
                    self.user.as_ref().is_some_and(|user| user.id != member.user.id);
                body = body.child(div().id(format!("member-{}", member.user.id))
                    .h(px(43.)).px_2().rounded_md().flex().items_center().gap_3()
                    .when(can_manage, |el| el.cursor_pointer())
                    .child(div().size(px(31.)).rounded_full().bg(rgb(color))
                        .flex().items_center().justify_center().text_sm()
                        .font_weight(FontWeight::BOLD).child(avatar))
                    .child(div().text_sm().text_color(rgb(theme_color("text")))
                        .child(username))
                    .when(can_manage, |el| el.on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_member = Some(target.clone());
                        this.modal = Modal::MemberActions;
                        cx.notify();
                    }))));
            }
        }
        body.into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        div().flex_1().bg(rgb(theme_color("content"))).flex().flex_col().items_center().justify_center().gap_5()
            .child(div().text_size(px(30.)).font_weight(FontWeight::BOLD).child("Make a space of your own"))
            .child(div().text_sm().text_color(rgb(theme_color("muted"))).child("Create a server or join one with an invite code."))
            .child(div().flex().gap_3()
                .child(div().id("empty-create").px_5().py_3().rounded_md().bg(rgb(theme_color("accent"))).cursor_pointer().child("Create server")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateServer; cx.notify(); })))
                .child(div().id("empty-join").px_5().py_3().rounded_md().bg(rgb(theme_color("join_button"))).cursor_pointer().child("Join server")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::JoinServer; cx.notify(); }))))
            .into_any_element()
    }

    fn render_search_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut results = div().id("search-results").h(px(390.)).min_h_0()
            .overflow_y_scrollbar().flex().flex_col().gap_2();
        for hit in &self.search_results {
            let channel = hit.message.channel_id;
            results = results.child(div().id(format!("search-result-{}",hit.message.id))
                .p_3().rounded_md().bg(rgb(theme_color("input_bg")))
                .cursor_pointer().flex().flex_col().gap_2()
                .child(div().flex().items_center().justify_between()
                    .child(div().text_sm().font_weight(FontWeight::BOLD)
                        .child(format!("#{}  /  {}",hit.channel_name,hit.message.author.username)))
                    .child(div().text_xs().text_color(rgb(theme_color("muted")))
                        .child(hit.message.created_at.format("%b %-d, %Y").to_string())))
                .child(div().text_sm().text_color(rgb(theme_color("message_text")))
                    .child(hit.message.content.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.modal = Modal::None;
                    if this.selected_channel != Some(channel) {
                        this.selected_channel = None;
                        this.select_channel(channel, cx);
                    } else {
                        this.issue(Command::SelectChannel(channel));
                    }
                })));
        }
        if self.search_results.is_empty() {
            results = results.child(div().p_4().text_sm().text_color(rgb(theme_color("muted")))
                .child("Search messages across this server"));
        }
        let card = div().w(px(650.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border")))
            .p_5().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child("Search"))
                .child(div().id("close-search").cursor_pointer().text_sm().child("Close")
                    .on_click(cx.listener(|this, _, _, cx| {this.dismiss_modal(cx);cx.notify();}))))
            .child(div().h(px(46.)).flex().gap_2()
                .child(div().flex_1().child(input_frame(&self.search_input)))
                .child(div().id("search-submit").w(px(95.)).rounded_md()
                    .bg(rgb(theme_color("accent"))).cursor_pointer()
                    .flex().items_center().justify_center().text_sm().child("Search")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(server)=this.selected_server {
                            let query=this.search_input.read(cx).value().to_string();
                            this.busy=true;
                            this.issue(Command::Search {server,query});
                            cx.notify();
                        }
                    }))))
            .child(results)
            .when_some(self.error.as_ref(),|el,error|el.child(div().text_sm()
                .text_color(rgb(theme_color("danger_text"))).child(error.clone())));
        div().absolute().inset_0().bg(rgba(theme_color("overlay")))
            .flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }

    fn render_pins_panel(&self,cx:&mut Context<Self>)->AnyElement {
        let mut rows=div().id("pins-scroll").h(px(360.)).min_h_0().overflow_y_scrollbar()
            .flex().flex_col().gap_2();
        for message in &self.pinned_messages {
            let channel=message.channel_id;
            rows=rows.child(div().id(format!("pinned-{}",message.id))
                .p_3().bg(rgb(theme_color("input_bg"))).rounded_md()
                .flex().flex_col().gap_2()
                .child(div().flex().items_center().justify_between()
                    .child(div().text_sm().font_weight(FontWeight::BOLD)
                        .child(message.author.username.clone()))
                    .child(div().text_xs().text_color(rgb(theme_color("muted")))
                        .child(message.created_at.format("%b %-d").to_string())))
                .child(div().text_sm().child(message.content.clone()))
                .child(div().id(format!("open-pin-{}",message.id)).cursor_pointer()
                    .text_xs().text_color(rgb(theme_color("link"))).child("Open channel")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.modal=Modal::None;
                        if this.selected_channel!=Some(channel){
                            this.selected_channel=None;
                            this.select_channel(channel,cx);
                        }
                    }))));
        }
        if self.pinned_messages.is_empty(){
            rows=rows.child(div().p_4().text_sm().text_color(rgb(theme_color("muted")))
                .child("No pinned messages"));
        }
        let card=div().w(px(520.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border")))
            .p_5().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child("Pinned Messages"))
                .child(div().id("close-pins").cursor_pointer().text_sm().child("Close")
                    .on_click(cx.listener(|this, _, _, cx|{this.dismiss_modal(cx);cx.notify();}))))
            .child(rows);
        div().absolute().inset_0().bg(rgba(theme_color("overlay")))
            .flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }

    fn render_inbox_panel(&self,cx:&mut Context<Self>)->AnyElement {
        let unread=self.notifications.iter().filter(|item|item.read_at.is_none()).count();
        let mut entries=div().id("notification-scroll").h(px(400.)).min_h_0()
            .overflow_y_scrollbar().flex().flex_col().gap_2();
        for item in &self.notifications {
            let message=match item.kind.as_str(){
                "mention"=>format!("{} mentioned you",item.actor.username),
                "direct_message"=>format!("New message from {}",item.actor.username),
                "friend_request"=>format!("Friend request from {}",item.actor.username),
                _=>item.kind.clone(),
            };
            let thread=item.thread_id;
            let channel=item.channel_id;
            entries=entries.child(div().id(format!("notice-{}",item.id))
                .p_3().rounded_md().bg(rgb(if item.read_at.is_none(){
                    theme_color("selection")
                }else{theme_color("input_bg")}))
                .flex().items_center().justify_between()
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(message))
                    .child(div().text_xs().text_color(rgb(theme_color("muted")))
                        .child(item.created_at.format("%b %-d, %H:%M").to_string())))
                .child(div().id(format!("notice-open-{}",item.id)).cursor_pointer()
                    .text_xs().text_color(rgb(theme_color("link"))).child("Open")
                    .on_click(cx.listener(move |this, _, _, cx|{
                        this.modal=Modal::None;
                        if let Some(thread)=thread{
                            this.select_direct(thread,cx);
                        }else if let Some(channel)=channel{
                            this.issue(Command::LocateChannel(channel));
                        }else{
                            this.enter_direct(cx);
                        }
                    }))));
        }
        if self.notifications.is_empty(){
            entries=entries.child(div().p_5().text_sm().text_color(rgb(theme_color("muted")))
                .child("No notifications yet"));
        }
        let card=div().w(px(540.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border")))
            .p_5().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD)
                    .child(format!("Inbox ({unread})")))
                .child(div().id("close-inbox").cursor_pointer().text_sm().child("Close")
                    .on_click(cx.listener(|this,_,_,cx|{this.dismiss_modal(cx);cx.notify();}))))
            .child(entries)
            .child(div().id("mark-notifications").h(px(37.)).rounded_md()
                .bg(rgb(theme_color("server_action_bg"))).cursor_pointer()
                .flex().items_center().justify_center().text_sm()
                .child("Mark all as read")
                .on_click(cx.listener(|this,_,_,cx|{
                    this.issue(Command::MarkNotificationsRead);
                    cx.notify();
                })));
        div().absolute().inset_0().bg(rgba(theme_color("overlay")))
            .flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }

    fn render_profile_panel(&self,cx:&mut Context<Self>)->AnyElement{
        let mut statuses=div().flex().flex_wrap().gap_2();
        for (key,label) in [("online","Online"),("idle","Idle"),("dnd","Do Not Disturb"),("invisible","Invisible")]{
            let active=self.profile_status==key;
            let status=key.to_owned();
            statuses=statuses.child(div().id(format!("status-{key}"))
                .px_3().py_2().rounded_md().cursor_pointer()
                .bg(rgb(if active{theme_color("accent")}else{theme_color("input_bg")}))
                .text_sm().child(label)
                .on_click(cx.listener(move |this,_,_,cx|{
                    this.profile_status=status.clone();
                    cx.notify();
                })));
        }
        let name=self.user.as_ref().map(|user|user.username.clone()).unwrap_or_default();
        let card=div().w(px(510.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border")))
            .p_6().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child("Profile"))
                .child(div().id("close-profile").cursor_pointer().text_sm().child("Close")
                    .on_click(cx.listener(|this,_,_,cx|{this.dismiss_modal(cx);cx.notify();}))))
            .child(div().text_sm().font_weight(FontWeight::BOLD).child(name))
            .child(div().text_xs().text_color(rgb(theme_color("muted"))).child("ABOUT ME"))
            .child(input_frame(&self.bio_input))
            .child(div().text_xs().text_color(rgb(theme_color("muted"))).child("AVAILABILITY"))
            .child(statuses)
            .child(div().flex().items_center().gap_3()
                .child(div().id("save-profile").flex_1().h(px(40.)).rounded_md()
                    .bg(rgb(theme_color("accent"))).cursor_pointer()
                    .flex().items_center().justify_center().text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.busy{"Saving"}else{"Save changes"})
                    .on_click(cx.listener(|this,_,_,cx|{
                        if this.busy{return;}
                        let bio=this.bio_input.read(cx).value().to_string();
                        let status=this.profile_status.clone();
                        this.busy=true;
                        this.issue(Command::UpdateProfile {bio,status});
                        cx.notify();
                    })))
                .child(div().id("logout-trigger").h(px(40.)).px_4().rounded_md()
                    .bg(rgb(theme_color("input_bg"))).cursor_pointer()
                    .flex().items_center().justify_center().text_sm()
                    .child("Log out")
                    .on_click(cx.listener(|this,_,_,cx|{
                        this.issue(Command::Logout);
                        cx.notify();
                    }))));
        div().absolute().inset_0().bg(rgba(theme_color("overlay")))
            .flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }

    fn render_member_actions(&self,cx:&mut Context<Self>)->AnyElement{
        let Some(member)=self.selected_member.as_ref() else {
            return div().into_any_element();
        };
        let target=member.user.id;
        let server=self.selected_server;
        let role=self.members.iter().find(|item|self.user.as_ref().is_some_and(|user|item.user.id==user.id))
            .map(|item|item.role.as_str()).unwrap_or("member");
        let can_change=role=="owner" && member.role!="owner";
        let can_remove=(role=="owner"&&member.role!="owner")||
            (role=="moderator"&&member.role=="member");
        let mut actions=div().flex().flex_col().gap_2();
        if can_change{
            let next=if member.role=="moderator"{"member"}else{"moderator"}.to_owned();
            let label=if member.role=="moderator"{"Remove moderator role"}else{"Make moderator"};
            actions=actions.child(div().id("member-role-action").h(px(40.)).px_3()
                .rounded_md().bg(rgb(theme_color("input_bg"))).cursor_pointer()
                .flex().items_center().text_sm().child(label)
                .on_click(cx.listener(move |this,_,_,cx|{
                    if let Some(server)=server{
                        this.busy=true;
                        this.issue(Command::SetMemberRole{server,user:target,role:next.clone()});
                        cx.notify();
                    }
                })));
        }
        if can_remove{
            actions=actions.child(div().id("member-kick-action").h(px(40.)).px_3()
                .rounded_md().bg(rgb(theme_color("input_bg"))).cursor_pointer()
                .flex().items_center().text_sm().child("Kick from server")
                .on_click(cx.listener(|this,_,_,cx|{
                    this.modal=Modal::KickMember;
                    cx.notify();
                })));
            actions=actions.child(div().id("member-ban-action").h(px(40.)).px_3()
                .rounded_md().bg(rgb(theme_color("input_bg"))).cursor_pointer()
                .flex().items_center().text_sm().text_color(rgb(theme_color("danger_text")))
                .child("Ban from server")
                .on_click(cx.listener(|this,_,_,cx|{
                    this.modal=Modal::BanMember;
                    cx.notify();
                })));
        }
        let card=div().w(px(420.)).bg(rgb(theme_color("sidebar"))).rounded_lg()
            .p_5().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(member.user.username.clone()))
                .child(div().id("close-member-actions").cursor_pointer().text_sm().child("Close")
                    .on_click(cx.listener(|this,_,_,cx|{this.dismiss_modal(cx);cx.notify();}))))
            .child(div().text_sm().text_color(rgb(theme_color("muted")))
                .child(format!("Role: {}",member.role)))
            .child(actions);
        div().absolute().inset_0().bg(rgba(theme_color("overlay")))
            .flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.modal {
            Modal::Search => return self.render_search_panel(cx),
            Modal::Pinned => return self.render_pins_panel(cx),
            Modal::Inbox => return self.render_inbox_panel(cx),
            Modal::Profile => return self.render_profile_panel(cx),
            Modal::MemberActions => return self.render_member_actions(cx),
            _ => {}
        }
        let (title, input, action) = match self.modal {
            Modal::CreateServer => ("Create server", Some(&self.server_name), "CREATE"),
            Modal::CreateChannel => ("Create channel", Some(&self.channel_name), "CREATE"),
            Modal::JoinServer => ("Join server", Some(&self.invite_input), "JOIN"),
            Modal::Invite => ("Invite to server", None, "COPY CODE"),
            Modal::CreateDirect => ("New direct message", Some(&self.person_input), "START"),
            Modal::AddFriend => ("Add friend", Some(&self.person_input), "SEND REQUEST"),
            Modal::EditMessage => ("Edit message", Some(&self.edit_input), "SAVE"),
            Modal::DeleteMessage => ("Delete message", None, "DELETE"),
            Modal::KickMember => ("Kick member", None, "KICK"),
            Modal::BanMember => ("Ban member", Some(&self.ban_reason), "BAN"),
            Modal::RenameServer => ("Rename server", Some(&self.rename_input), "SAVE"),
            Modal::RenameChannel => ("Rename channel", Some(&self.rename_input), "SAVE"),
            Modal::Search | Modal::Pinned | Modal::Inbox | Modal::Profile | Modal::MemberActions | Modal::None => ("", None, ""),
        };
        let mut card = div().w(px(440.)).bg(rgb(theme_color("sidebar"))).rounded_lg().border_1().border_color(rgb(theme_color("border"))).p_6().flex().flex_col().gap_5()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                .child(div().id("modal-close").text_sm().text_color(rgb(theme_color("muted"))).cursor_pointer().child("CLOSE")
                    .on_click(cx.listener(|this, _, _, cx| { this.dismiss_modal(cx); this.error = None; cx.notify(); }))));
        if let Some(input) = input {
            card = card.child(input_frame(input));
        } else if self.modal == Modal::Invite {
            card = card.child(div().p_4().rounded_md().bg(rgb(theme_color("input_bg")))
                .text_sm().child(self.invite_code.clone()));
        } else {
            let warning = match self.modal {
                Modal::KickMember => "This member will be removed from the server.",
                Modal::DeleteMessage => "This will permanently remove the message.",
                _ => "",
            };
            card = card.child(div().text_sm().text_color(rgb(theme_color("muted")))
                .child(warning));
        }
        if let Some(error) = &self.error {
            card = card.child(div().text_sm().text_color(rgb(theme_color("danger_text"))).child(error.clone()));
        }
        card = card.child(div().id("modal-submit").h(px(43.)).rounded_md()
            .bg(rgb(if matches!(self.modal, Modal::DeleteMessage | Modal::KickMember | Modal::BanMember) {
                theme_color("delete_action")
            } else {
                theme_color("accent")
            })).flex().items_center()
            .justify_center().cursor_pointer().font_weight(FontWeight::SEMIBOLD).text_sm()
            .child(if self.busy { "WORKING" } else { action })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.busy { return; }
                this.error = None;
                match this.modal {
                    Modal::CreateServer => {
                        let name = this.server_name.read(cx).value().to_string();
                        this.busy = true;
                        this.issue(Command::CreateServer(name));
                    }
                    Modal::CreateChannel => {
                        if let Some(server) = this.selected_server {
                            let name = this.channel_name.read(cx).value().to_string();
                            this.busy = true;
                            this.issue(Command::CreateChannel { server, name });
                        }
                    }
                    Modal::JoinServer => {
                        let code = this.invite_input.read(cx).value().to_string();
                        this.busy = true;
                        this.issue(Command::JoinInvite(code));
                    }
                    Modal::Invite => {
                        cx.write_to_clipboard(ClipboardItem::new_string(this.invite_code.clone()));
                        this.modal = Modal::None;
                    }
                    Modal::CreateDirect => {
                        let username = this.person_input.read(cx).value().to_string();
                        this.busy = true;
                        this.issue(Command::CreateDirect(username));
                    }
                    Modal::AddFriend => {
                        let username = this.person_input.read(cx).value().to_string();
                        this.busy = true;
                        this.issue(Command::AddFriend(username));
                    }
                    Modal::EditMessage => {
                        if let (Some(channel), Some(message)) = (this.selected_channel, this.editing_message) {
                            let content = this.edit_input.read(cx).value().to_string();
                            let direct = this.direct_mode;
                            this.busy = true;
                            this.issue(Command::EditMessage { channel, message, direct, content });
                        }
                    }
                    Modal::DeleteMessage => {
                        if let Some((channel, message, direct)) = this.pending_delete {
                            this.busy = true;
                            this.issue(Command::DeleteMessage { channel, message, direct });
                        }
                    }
                    Modal::RenameServer => {
                        if let Some(server) = this.selected_server {
                            let name = this.rename_input.read(cx).value().to_string();
                            this.busy = true;
                            this.issue(Command::RenameServer {server,name});
                        }
                    }
                    Modal::RenameChannel => {
                        if let (Some(server),Some(channel))=(this.selected_server,this.selected_channel) {
                            let name=this.rename_input.read(cx).value().to_string();
                            this.busy=true;
                            this.issue(Command::RenameChannel {server,channel,name});
                        }
                    }
                    Modal::KickMember => {
                        if let (Some(server),Some(member))=(this.selected_server,this.selected_member.as_ref()) {
                            let user=member.user.id;
                            this.busy=true;
                            this.issue(Command::KickMember {server,user});
                        }
                    }
                    Modal::BanMember => {
                        if let (Some(server),Some(member))=(this.selected_server,this.selected_member.as_ref()) {
                            let user=member.user.id;
                            let reason=this.ban_reason.read(cx).value().to_string();
                            this.busy=true;
                            this.issue(Command::BanMember {server,user,reason});
                        }
                    }
                    Modal::Search | Modal::Pinned | Modal::Inbox | Modal::Profile | Modal::MemberActions | Modal::None => {}
                }
                cx.notify();
            })));
        div().absolute().inset_0().bg(rgba(theme_color("overlay"))).flex().items_center().justify_center().child(card.with_animation(
                format!("dialog-card-{}-{}", self.modal as u32,
                    self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal)),
                entrance(190),
                |el, t| el.mt(px((1.0 - t) * 14.0))
            )).into_any_element()
    }
}

impl Render for UTAchat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.user.is_none() {
            return self.render_auth(cx);
        }
        let in_server = !self.direct_mode && self.selected_server.is_some();
        let member_width = gpui_kit::base::transition(
            "utachat-member-panel-width",
            if in_server && self.show_members { 240.0_f32 } else { 0.0_f32 },
            gpui_kit::base::Transition::new(Duration::from_millis(210))
                .easing(gpui_kit::base::Easing::EaseOut),
            window,
            cx,
        );
        let chat_identity = format!("chat-view-{}-{}", self.direct_mode,
            self.selected_channel.map(|id| id.to_string()).unwrap_or_default());
        let sidebar_identity = format!("sidebar-{}",
            self.selected_server.map(|id| id.to_string()).unwrap_or_default());
        let mut app = div().relative().size_full().flex().flex_col()
            .bg(rgb(theme_color("content"))).text_color(rgb(theme_color("text")))
            .font_family("Segoe UI").text_size(px(14.))
            .child(div().flex_1().min_h_0().flex()
                .child(self.render_server_rail(cx))
                .when(self.direct_mode, |row| row.child(
                    div().w(px(240.)).h_full().child(self.render_direct_sidebar(cx))
                        .with_animation("direct-sidebar", entrance(180),
                            |el, t| el.opacity(0.5 + 0.5 * t))))
                .when(in_server, |row| row.child(
                    div().w(px(240.)).h_full().child(self.render_channel_sidebar(cx))
                        .with_animation(sidebar_identity, entrance(180),
                            |el, t| el.opacity(0.5 + 0.5 * t))))
                .when(self.direct_mode || self.selected_server.is_some(), |row| row.child(
                    div().flex_1().min_w_0().h_full().child(self.render_chat(cx))
                        .with_animation(chat_identity, entrance(160),
                            |el, t| el.opacity(0.65 + 0.35 * t))))
                .when(in_server && member_width > 0.5, |row| row.child(
                    div().h_full().w(px(member_width)).min_w_0().overflow_hidden()
                        .child(self.render_members(cx))))
                .when(!self.direct_mode && self.selected_server.is_none(), |row|
                    row.child(self.render_empty(cx))));
        if let Some(error) = &self.error {
            app = app.child(div().absolute().top(px(16.)).right(px(20.)).w(px(360.)).p_3()
                .bg(rgb(theme_color("toast_bg"))).rounded_lg().flex().items_center()
                .justify_between().gap_3()
                .child(div().flex_1().text_sm().child(error.clone()))
                .child(div().id("dismiss-error").text_xs().cursor_pointer()
                    .text_color(rgb(theme_color("text"))).child("Dismiss")
                    .on_click(cx.listener(|this, _, _, cx| { this.error = None; cx.notify(); })))
                .with_animation("error-notice-enter", entrance(180),
                    |el, t| el.opacity(t).mt(px((1. - t) * 9.))));
        }
        if self.modal != Modal::None {
            let leaving = self.modal_close_at.is_some_and(|(kind, _)| kind == self.modal);
            let key = format!("dialog-{}-{leaving}", self.modal as u32);
            app = app.child(div().absolute().inset_0()
                .child(self.render_modal(cx))
                .with_animation(key, entrance(if leaving { 160 } else { 190 }),
                    move |el, t| el.opacity(if leaving { 1.0 - t } else { t })));
        }
        app.into_any_element()
    }
}

fn input_frame(state: &Entity<InputState>) -> AnyElement {
    div().h(px(44.)).px_3().bg(rgb(theme_color("input_bg"))).rounded_md().flex().items_center().text_size(px(14.))
        .child(Input::new(state).appearance(false).bordered(false).w_full())
        .into_any_element()
}

fn render_message(message: &ChatMessage, continuation: bool, current_user: Option<Uuid>, direct: bool, expanded: bool, hovered: bool, can_pin: bool, cx: &mut Context<UTAchat>) -> AnyElement {
    let avatar = initials(&message.author.username);
    let color = avatar_color(message.author.id);
    let channel = message.channel_id;
    let id = message.id;
    let is_author = current_user == Some(message.author.id);
    let mut actions = div().flex().items_center().gap_4().px_2().py_1();
    actions = actions.child(div().id(format!("reply-{id}")).text_xs().text_color(rgb(theme_color("link")))
        .cursor_pointer().child("Reply").on_click(cx.listener(move |this, _, _, cx| {
            this.reply_to = Some(id);
            cx.notify();
        })));
    if is_author {
        let current_text = message.content.clone();
        actions = actions.child(div().id(format!("edit-{id}")).text_xs().text_color(rgb(theme_color("muted")))
            .cursor_pointer().child("Edit").on_click(cx.listener(move |this, _, window, cx| {
                this.editing_message = Some(id);
                this.edit_input.update(cx, |input, cx| input.set_value(current_text.clone(), window, cx));
                this.modal = Modal::EditMessage;
                cx.notify();
            })));
        actions = actions.child(div().id(format!("delete-{id}")).text_xs().text_color(rgb(theme_color("delete_action")))
            .cursor_pointer().child("Delete").on_click(cx.listener(move |this, _, _, cx| {
                this.pending_delete = Some((channel, id, direct));
                this.modal = Modal::DeleteMessage;
                this.action_message = None;
                cx.notify();
            })));
    }

    if can_pin {
        let pinned = message.pinned_at.is_some();
        actions = actions.child(div().id(format!("pin-{id}"))
            .cursor_pointer().text_xs().text_color(rgb(theme_color("muted")))
            .child(if pinned { "Unpin" } else { "Pin" })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.issue(Command::SetPin { channel, message: id, pinned: !pinned });
                this.action_message = None;
                cx.notify();
            })));
    }
    if !direct {
        actions = actions.child(div().id(format!("react-{id}")).text_xs().text_color(rgb(theme_color("muted")))
            .cursor_pointer().child("Like").on_click(cx.listener(move |this, _, _, cx| {
                this.issue(Command::React { channel, message: id, kind: "like".to_owned() });
                cx.notify();
            })));
    }
    let menu_button = div().id(format!("message-options-{id}"))
        .size(px(32.)).rounded_md()
        .cursor_pointer().flex().items_center().justify_center()
        .child(svg().path("icons/ellipsis.svg").size(px(19.))
            .text_color(rgb(theme_color("text"))))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.action_message = if this.action_message == Some(id) { None } else { Some(id) };
            cx.notify();
        }));
    let mut toolbar = div().absolute().top(px(-14.)).right(px(14.))
        .px_1().py_1().rounded_md().border_1().border_color(rgb(theme_color("border")))
        .bg(rgb(theme_color("toolbar_bg")))
        .flex().items_center().gap_1()
        .child(div().id(format!("quick-reply-{id}")).size(px(32.)).rounded_md()
            .cursor_pointer().flex().items_center().justify_center()
            .child(svg().path("icons/reply.svg").size(px(18.))
                .text_color(rgb(theme_color("muted"))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reply_to = Some(id);
                this.action_message = None;
                cx.notify();
            })));
    if !direct {
        toolbar = toolbar.child(div().id(format!("quick-react-{id}"))
            .size(px(32.)).rounded_md().cursor_pointer()
            .flex().items_center().justify_center()
            .child(svg().path("icons/reaction.svg").size(px(18.))
                .text_color(rgb(theme_color("muted"))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.issue(Command::React {
                    channel, message: id, kind: "like".to_owned()
                });
                cx.notify();
            })));
    }
    toolbar = toolbar.child(menu_button);
    let mut body = div().flex_1().min_w_0().flex().flex_col().gap_1()
        .when(!continuation, |el| el.child(div().flex().items_center().gap_3()
            .child(div().text_size(px(14.5)).font_weight(FontWeight::BOLD).child(message.author.username.clone()))
            .child(div().text_size(px(11.5)).text_color(rgb(theme_color("subtle"))).child(message.created_at.format("%d %b %Y, %H:%M").to_string()))
            .when(message.edited_at.is_some(), |el| el.child(div().text_xs().text_color(rgb(theme_color("subtle"))).child("edited")))
            .when(message.pinned_at.is_some(), |el| el.child(div().text_xs()
                .text_color(rgb(theme_color("link"))).child("Pinned")))))
        .when_some(message.reply_to, |el, replied| el.child(div().text_xs().text_color(rgb(theme_color("link")))
            .child(format!("Reply to {}", &replied.to_string()[..8]))))
        .child(div().min_w_0().text_size(px(15.))
            .text_color(rgb(theme_color("message_text"))).child(message.content.clone()))
        .when(expanded, |el| el.child(actions));
    if !message.reactions.is_empty() {
        for reaction in &message.reactions {
            body = body.child(div().px_2().py_1().rounded_md().bg(rgb(theme_color("selection")))
                .text_xs().child(format!("{} {}", reaction.kind, reaction.count)));
        }
    }
    div().id(format!("message-row-{id}")).relative().w_full()
        .px_4().py_2().flex().items_start().gap_3()
        .bg(rgb(if hovered || expanded {
            theme_color("message_hover")
        } else {
            theme_color("content")
        }))
        .child(div().w(px(40.)).flex().justify_center()
            .when(!continuation, |el| el.child(div().size(px(38.)).rounded_full().bg(rgb(color))
                .flex().items_center().justify_center().font_weight(FontWeight::BOLD)
                .text_size(px(13.)).child(avatar))))
        .child(body)
        .when(hovered || expanded, |el| el.child(toolbar.with_animation(
            format!("toolbar-entrance-{id}"),
            entrance(120),
            |el, t| el.opacity(t).mt(px((1. - t) * 5.))
        )))
        .on_hover(cx.listener(move |this, &inside, _, cx| {
            if inside {
                if this.hovered_message != Some(id) {
                    this.hovered_message = Some(id);
                    cx.notify();
                }
            } else if this.hovered_message == Some(id) {
                this.hovered_message = None;
                cx.notify();
            }
        }))
        .with_animation(format!("message-enter-{id}"), entrance(180),
            |el, t| el.opacity(0.55 + t * 0.45))
        .into_any_element()
}

fn entrance(milliseconds: u64) -> Animation {
    Animation::new(Duration::from_millis(milliseconds)).with_easing(ease_out_quint())
}

fn blend_color(from: u32, to: u32, amount: f32) -> u32 {
    let t = amount.clamp(0.0, 1.0);
    let channel = |shift: u32| -> u32 {
        let a = ((from >> shift) & 255) as f32;
        let b = ((to >> shift) & 255) as f32;
        ((a + (b - a) * t).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

fn initials(name: &str) -> String {
    name.chars().take(2).collect::<String>().to_uppercase()
}

fn avatar_color(user: Uuid) -> u32 {
    let colors: [u32; 8] = [theme_color("avatar_0"), theme_color("avatar_1"), theme_color("avatar_2"), theme_color("avatar_3"), theme_color("avatar_4"), theme_color("avatar_5"), theme_color("avatar_6"), theme_color("avatar_7")];
    colors[(user.as_u128() % colors.len() as u128) as usize]
}

use std::{sync::mpsc::TryRecvError, time::Duration};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use uuid::Uuid;
use utachat_shared::{AuthResponse, Channel, ChatMessage, DirectThread, Friend, GatewayMessage, Member, PublicUser, Server};
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
}

pub struct UTAchat {
    net: Network,
    user: Option<PublicUser>,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
    directs: Vec<DirectThread>,
    friends: Vec<Friend>,
    direct_mode: bool,
    show_members: bool,
    action_message: Option<Uuid>,
    reply_to: Option<Uuid>,
    editing_message: Option<Uuid>,
    pending_delete: Option<(Uuid, Uuid, bool)>,
    messages: Vec<ChatMessage>,
    chat_scroll: ScrollHandle,
    selected_server: Option<Uuid>,
    selected_channel: Option<Uuid>,
    connected: bool,
    register: bool,
    busy: bool,
    sending: bool,
    pending_message: Option<String>,
    has_older: bool,
    modal: Modal,
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
            show_members: true, action_message: None,
            reply_to: None, editing_message: None, pending_delete: None,
            chat_scroll: ScrollHandle::new(),
            selected_server: None, selected_channel: None,
            connected: false, register: false, busy: false, sending: false,
            pending_message: None, has_older: false, modal: Modal::None,
            invite_code: String::new(), error: None,
            username, password, composer, server_name, channel_name, invite_input,
            person_input, edit_input,
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

    fn select_channel(&mut self, channel: Uuid, cx: &mut Context<Self>) {
        if self.selected_channel == Some(channel) { return; }
        self.selected_channel = Some(channel);
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
                    self.direct_mode = true;
                    self.reply_to = None;
                    self.members.clear();
                    self.messages.clear();
                    self.selected_server = None;
                    self.selected_channel = None;
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
                        if let Some(channel) = self.channels.first().map(|c| c.id) {
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
                UiEvent::Gateway(GatewayMessage::MessageCreated { message })
                | UiEvent::Gateway(GatewayMessage::DirectMessageCreated { message }) => self.push_message(message),
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
                UiEvent::Gateway(GatewayMessage::ChannelCreated { channel }) => {
                    if Some(channel.server_id) == self.selected_server && !self.channels.iter().any(|c| c.id == channel.id) {
                        self.channels.push(channel);
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
        if changed { cx.notify(); }
    }

    fn render_auth(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = if self.register { "Create account" } else { "Sign in" };
        div()
            .size_full().bg(rgb(theme_color("content"))).font_family("Segoe UI").text_color(rgb(theme_color("text"))).flex().items_center().justify_center()
            .child(
                div().w(px(420.)).bg(rgb(theme_color("sidebar"))).rounded_lg().p_8().flex().flex_col().gap_5()
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
        let mut rail = div().id("server-rail").w(px(72.)).h_full().min_h_0().overflow_y_scrollbar().bg(rgb(theme_color("rail"))).py_3().flex().flex_col().items_center().gap_3()
            .child(div().id("home-direct").size(px(46.))
                .when(self.direct_mode, |el| el.rounded_lg())
                .when(!self.direct_mode, |el| el.rounded_full())
                .bg(rgb(if self.direct_mode { theme_color("accent") } else { theme_color("server_inactive") }))
                .flex().items_center().justify_center().cursor_pointer()
                .font_weight(FontWeight::BOLD).text_base().child(svg().path("icons/utachat-mark.svg").size(px(30.)))
                .on_click(cx.listener(|this, _, _, cx| this.enter_direct(cx))))
            .child(div().h(px(2.)).w(px(34.)).bg(rgb(theme_color("line"))));
        for server in &self.servers {
            let id = server.id;
            let name = server.name.chars().take(2).collect::<String>().to_uppercase();
            let active = !self.direct_mode && Some(id) == self.selected_server;
            rail = rail.child(
                div().id(format!("server-{id}")).size(px(46.))
                    .when(active, |el| el.rounded_lg())
                    .when(!active, |el| el.rounded_full())
                    .flex().items_center().justify_center()
                    .bg(rgb(if active { theme_color("accent") } else { theme_color("server_inactive") }))
                    .text_color(rgb(theme_color("text"))).font_weight(FontWeight::SEMIBOLD).text_sm()
                    .cursor_pointer().child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_server(id, cx)))
            );
        }
        rail.child(div().id("create-server-trigger").size(px(44.)).rounded_full().bg(rgb(theme_color("server_action_bg")))
            .flex().items_center().justify_center().text_color(rgb(theme_color("server_add"))).text_xl().cursor_pointer()
            .child("+").on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateServer; this.error = None; cx.notify(); })))
            .child(div().id("join-server-trigger").size(px(44.)).rounded_full().bg(rgb(theme_color("server_action_bg")))
            .flex().items_center().justify_center().text_color(rgb(theme_color("muted"))).text_sm().cursor_pointer()
            .child(svg().path("icons/compass.svg").size(px(23.)))
            .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::JoinServer; this.error = None; cx.notify(); })))
            .into_any_element()
    }

    fn render_user_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let username = self.user.as_ref().map(|user| user.username.clone()).unwrap_or_default();
        div().h(px(60.)).w_full().px_3().bg(rgb(theme_color("profile_bar")))
            .flex().items_center().justify_between()
            .child(div().flex().items_center().gap_2()
                .child(div().relative().size(px(34.)).rounded_full()
                    .bg(rgb(theme_color("avatar_0"))).flex().items_center().justify_center()
                    .text_sm().font_weight(FontWeight::BOLD).child(initials(&username))
                    .child(div().absolute().right(px(0.)).bottom(px(0.)).size(px(10.))
                        .rounded_full().bg(rgb(if self.connected { theme_color("online") } else { theme_color("reconnecting") }))))
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(username))
                    .child(div().text_xs().text_color(rgb(theme_color("muted")))
                        .child(if self.connected { "Online" } else { "Connecting" }))))
            .child(div().id("logout-trigger").px_2().py_2().rounded_md()
                .cursor_pointer().text_xs().text_color(rgb(theme_color("muted")))
                .child("Log out")
                .on_click(cx.listener(|this, _, _, cx| this.issue(Command::Logout))))
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
        div().w(px(248.)).h_full().flex().flex_col().bg(rgb(theme_color("sidebar")))
            .child(div().h(px(56.)).px_4().border_b_1().border_color(rgb(theme_color("separator")))
                .flex().items_center().justify_between()
                .child(div().text_base().font_weight(FontWeight::BOLD).child("Direct Messages"))
                .child(div().id("new-direct").text_xs().text_color(rgb(theme_color("link"))).cursor_pointer()
                    .flex().items_center().gap_2().child(svg().path("icons/new-message.svg").size(px(16.))).child("NEW")
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
        let can_create = self.selected_server.and_then(|id| self.servers.iter().find(|s| s.id == id))
            .zip(self.user.as_ref()).is_some_and(|(s, u)| s.owner_id == u.id);
        let mut channel_list = div().id("channel-scroll").flex_1().min_h_0().overflow_y_scrollbar().p_3().flex().flex_col().gap_1()
            .child(div().h(px(30.)).px_2().flex().items_center().justify_between()
                .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted"))).child("TEXT CHANNELS"))
                .when(can_create, |header| header.child(div().id("create-channel-trigger").cursor_pointer()
                    .text_lg().text_color(rgb(theme_color("muted"))).child("+")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateChannel; this.error = None; cx.notify(); })))));
        for channel in &self.channels {
            let id = channel.id;
            let active = Some(id) == self.selected_channel;
            channel_list = channel_list.child(div().id(format!("channel-{id}")).h(px(34.)).px_3().rounded_md()
                .flex().items_center().gap_2().cursor_pointer()
                .bg(rgb(if active { theme_color("selection") } else { theme_color("sidebar") }))
                .text_color(rgb(if active { theme_color("text") } else { theme_color("muted") }))
                .child(div().text_lg().child("#"))
                .child(div().text_sm().font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(channel.name.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.select_channel(id, cx))));
        }
        div().w(px(248.)).h_full().flex().flex_col().bg(rgb(theme_color("sidebar")))
            .child(div().h(px(56.)).px_4().border_b_1().border_color(rgb(theme_color("separator")))
                .flex().items_center().justify_between()
                .child(div().text_base().font_weight(FontWeight::BOLD).child(server_name))
                .child(div().id("invite-trigger").cursor_pointer().text_xs().text_color(rgb(theme_color("muted")))
                    .child("INVITE").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(server) = this.selected_server {
                            this.busy = true;
                            this.issue(Command::FetchInvite(server));
                        }
                        cx.notify();
                    }))))
            .child(channel_list)
            .child(self.render_user_panel(cx))
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
        let mut stream = div().id("message-scroll").flex_1().min_h_0().track_scroll(&self.chat_scroll).overflow_y_scrollbar().px_5().py_4().flex().flex_col().gap_1();
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
        if self.messages.is_empty() && self.selected_channel.is_some() {
            stream = stream.child(div().flex_1().flex().flex_col().items_start()
                .justify_end().pb_5().gap_3()
                .child(div().size(px(64.)).rounded_full().bg(rgb(theme_color("selection")))
                    .flex().items_center().justify_center()
                    .text_size(px(30.)).font_weight(FontWeight::BOLD)
                    .child(if self.direct_mode { "@" } else { "#" }))
                .child(div().text_size(px(24.)).font_weight(FontWeight::BOLD)
                    .child(if self.direct_mode {
                        channel_name.to_string()
                    } else {
                        format!("Welcome to #{}", channel_name)
                    })));
        } else {
            for (i, message) in self.messages.iter().enumerate() {
                let continuation = i > 0 && self.messages[i - 1].author.id == message.author.id
                    && (message.created_at - self.messages[i - 1].created_at).num_minutes() < 5;
                let expanded = self.action_message == Some(message.id);
                stream = stream.child(render_message(message, continuation,
                    self.user.as_ref().map(|u| u.id), self.direct_mode, expanded, cx));
            }
        }
        let mut center = div().flex_1().min_w_0().h_full().flex().flex_col().bg(rgb(theme_color("content")))
            .child(div().h(px(56.)).bg(rgb(theme_color("header"))).border_b_1().border_color(rgb(theme_color("header_separator"))).px_5()
                .flex().items_center().justify_between()
                .child(div().flex().items_center().gap_3()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(rgb(theme_color("muted")))
                        .child(if self.direct_mode { "@" } else { "#" }))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(channel_name.to_string())))
                .child(div().flex().items_center().gap_3()
                    .when(!self.direct_mode, |el| el.child(div().id("members-toggle").px_2().py_2()
                        .cursor_pointer().rounded_md().flex().items_center().gap_2()
                        .child(svg().path("icons/users.svg").size(px(19.)))
                        .child(div().text_xs().text_color(rgb(theme_color("muted")))
                            .child(if self.show_members { "Hide members" } else { "Members" }))
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
                    .child(div().flex_1().min_w_0()
                        .child(Input::new(&self.composer).appearance(false).bordered(false).w_full()))
                    .child(div().id("send-trigger").size(px(36.)).rounded_md()
                        .bg(rgb(theme_color("accent")))
                        .cursor_pointer().flex().items_center().justify_center()
                        .child(svg().path("icons/send.svg").size(px(18.)))
                        .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx))))));
        }
        center.into_any_element()
    }

    fn render_members(&self) -> AnyElement {
        let mut body = div().id("member-scroll").w(px(240.)).h_full().min_h_0()
            .overflow_y_scrollbar().bg(rgb(theme_color("member_bg")))
            .p_3().flex().flex_col().gap_1();
        for (role, title) in [("owner", "SERVER OWNER"), ("member", "MEMBERS")] {
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
                body = body.child(div().h(px(43.)).px_2().rounded_md()
                    .flex().items_center().gap_3()
                    .child(div().size(px(31.)).rounded_full().bg(rgb(color))
                        .flex().items_center().justify_center().text_sm()
                        .font_weight(FontWeight::BOLD).child(avatar))
                    .child(div().text_sm().text_color(rgb(theme_color("text")))
                        .child(username)));
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

    fn render_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let (title, input, action) = match self.modal {
            Modal::CreateServer => ("Create server", Some(&self.server_name), "CREATE"),
            Modal::CreateChannel => ("Create channel", Some(&self.channel_name), "CREATE"),
            Modal::JoinServer => ("Join server", Some(&self.invite_input), "JOIN"),
            Modal::Invite => ("Invite to server", None, "COPY CODE"),
            Modal::CreateDirect => ("New direct message", Some(&self.person_input), "START"),
            Modal::AddFriend => ("Add friend", Some(&self.person_input), "SEND REQUEST"),
            Modal::EditMessage => ("Edit message", Some(&self.edit_input), "SAVE"),
            Modal::DeleteMessage => ("Delete message", None, "DELETE"),
            Modal::None => ("", None, ""),
        };
        let mut card = div().w(px(440.)).bg(rgb(theme_color("sidebar"))).rounded_lg().p_6().flex().flex_col().gap_5()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                .child(div().id("modal-close").text_sm().text_color(rgb(theme_color("muted"))).cursor_pointer().child("CLOSE")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::None; this.error = None; cx.notify(); }))));
        if let Some(input) = input {
            card = card.child(input_frame(input));
        } else if self.modal == Modal::Invite {
            card = card.child(div().p_4().rounded_md().bg(rgb(theme_color("input_bg")))
                .text_sm().child(self.invite_code.clone()));
        } else {
            card = card.child(div().text_sm().text_color(rgb(theme_color("muted")))
                .child("This will permanently remove the message."));
        }
        if let Some(error) = &self.error {
            card = card.child(div().text_sm().text_color(rgb(theme_color("danger_text"))).child(error.clone()));
        }
        card = card.child(div().id("modal-submit").h(px(43.)).rounded_md()
            .bg(rgb(if self.modal == Modal::DeleteMessage {
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
                    Modal::None => {}
                }
                cx.notify();
            })));
        div().absolute().inset_0().bg(rgba(theme_color("overlay"))).flex().items_center().justify_center().child(card).into_any_element()
    }
}

impl Render for UTAchat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.user.is_none() {
            return self.render_auth(cx);
        }
        let mut app = div().relative().size_full().flex().flex_col().bg(rgb(theme_color("content"))).text_color(rgb(theme_color("text")))
            .font_family("Segoe UI")
            .child(div().flex_1().min_h_0().flex()
                .child(self.render_server_rail(cx))
                .when(self.direct_mode, |row| row.child(self.render_direct_sidebar(cx)))
                .when(!self.direct_mode && self.selected_server.is_some(), |row| row.child(self.render_channel_sidebar(cx)))
                .when(self.direct_mode || self.selected_server.is_some(), |row| row.child(self.render_chat(cx)))
                .when(!self.direct_mode && self.selected_server.is_some() && self.show_members, |row| row.child(self.render_members()))
                .when(!self.direct_mode && self.selected_server.is_none(), |row| row.child(self.render_empty(cx))));
        if let Some(error) = &self.error {
            app = app.child(div().absolute().top(px(12.)).right(px(16.)).w(px(400.)).p_3()
                .bg(rgb(theme_color("toast_bg"))).rounded_md().flex().items_center().justify_between().gap_3()
                .child(div().flex_1().text_sm().child(error.clone()))
                .child(div().id("dismiss-error").text_xs().cursor_pointer().child("DISMISS")
                    .on_click(cx.listener(|this, _, _, cx| { this.error = None; cx.notify(); }))));
        }
        if self.modal != Modal::None {
            app = app.child(self.render_modal(cx));
        }
        app.into_any_element()
    }
}

fn input_frame(state: &Entity<InputState>) -> AnyElement {
    div().h(px(44.)).px_3().bg(rgb(theme_color("input_bg"))).rounded_md().flex().items_center()
        .child(Input::new(state).appearance(false).bordered(false).w_full())
        .into_any_element()
}

fn render_message(message: &ChatMessage, continuation: bool, current_user: Option<Uuid>, direct: bool, expanded: bool, cx: &mut Context<UTAchat>) -> AnyElement {
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
    if !direct {
        actions = actions.child(div().id(format!("react-{id}")).text_xs().text_color(rgb(theme_color("muted")))
            .cursor_pointer().child("Like").on_click(cx.listener(move |this, _, _, cx| {
                this.issue(Command::React { channel, message: id, kind: "like".to_owned() });
                cx.notify();
            })));
    }
    let menu_button = div().id(format!("message-options-{id}"))
        .size(px(28.)).rounded_md().bg(rgb(theme_color("selection")))
        .cursor_pointer().flex().items_center().justify_center()
        .child(svg().path("icons/ellipsis.svg").size(px(17.)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.action_message = if this.action_message == Some(id) { None } else { Some(id) };
            cx.notify();
        }));
    let mut body = div().flex_1().min_w_0().flex().flex_col().gap_1()
        .when(!continuation, |el| el.child(div().flex().items_center().gap_3()
            .child(div().text_sm().font_weight(FontWeight::BOLD).child(message.author.username.clone()))
            .child(div().text_xs().text_color(rgb(theme_color("subtle"))).child(message.created_at.format("%d %b %Y, %H:%M").to_string()))
            .when(message.edited_at.is_some(), |el| el.child(div().text_xs().text_color(rgb(theme_color("subtle"))).child("edited")))))
        .when_some(message.reply_to, |el, replied| el.child(div().text_xs().text_color(rgb(theme_color("link")))
            .child(format!("Reply to {}", &replied.to_string()[..8]))))
        .child(div().flex().items_start().justify_between().gap_3()
            .child(div().flex_1().min_w_0().text_sm()
                .text_color(rgb(theme_color("message_text"))).child(message.content.clone()))
            .child(menu_button))
        .when(expanded, |el| el.child(actions));
    if !message.reactions.is_empty() {
        for reaction in &message.reactions {
            body = body.child(div().px_2().py_1().rounded_md().bg(rgb(theme_color("selection")))
                .text_xs().child(format!("{} {}", reaction.kind, reaction.count)));
        }
    }
    div().w_full().px_1().py_1().flex().items_start().gap_3()
        .child(div().w(px(40.)).flex().justify_center()
            .when(!continuation, |el| el.child(div().size(px(36.)).rounded_full().bg(rgb(color))
                .flex().items_center().justify_center().font_weight(FontWeight::BOLD).text_sm().child(avatar))))
        .child(body).into_any_element()
}

fn initials(name: &str) -> String {
    name.chars().take(2).collect::<String>().to_uppercase()
}

fn avatar_color(user: Uuid) -> u32 {
    let colors: [u32; 8] = [theme_color("avatar_0"), theme_color("avatar_1"), theme_color("avatar_2"), theme_color("avatar_3"), theme_color("avatar_4"), theme_color("avatar_5"), theme_color("avatar_6"), theme_color("avatar_7")];
    colors[(user.as_u128() % colors.len() as u128) as usize]
}

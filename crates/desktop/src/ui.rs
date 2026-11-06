use std::{sync::mpsc::TryRecvError, time::Duration};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use uuid::Uuid;
use utachat_shared::{AuthResponse, Channel, ChatMessage, GatewayMessage, Member, PublicUser, Server};
use crate::network::{self, Command, Network, UiEvent};

const RAIL: u32 = 0x17181b;
const SIDEBAR: u32 = 0x222327;
const CONTENT: u32 = 0x303136;
const HEADER: u32 = 0x2b2c31;
const MEMBER_BG: u32 = 0x28292e;
const INPUT_BG: u32 = 0x383a42;
const TEXT: u32 = 0xf3f4f7;
const MUTED: u32 = 0xa6a8b0;
const SUBTLE: u32 = 0x747781;
const ACCENT: u32 = 0x5865f2;
const LINE: u32 = 0x3a3c43;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modal {
    None,
    CreateServer,
    CreateChannel,
    JoinServer,
    Invite,
}

pub struct UTAchat {
    net: Network,
    user: Option<PublicUser>,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
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
            chat_scroll: ScrollHandle::new(),
            selected_server: None, selected_channel: None,
            connected: false, register: false, busy: false, sending: false,
            pending_message: None, has_older: false, modal: Modal::None,
            invite_code: String::new(), error: None,
            username, password, composer, server_name, channel_name, invite_input,
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
        self.issue(Command::SendMessage { channel, content });
        cx.notify();
    }

    fn select_server(&mut self, server: Uuid, cx: &mut Context<Self>) {
        if self.selected_server == Some(server) { return; }
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
        self.messages.clear();
        self.chat_scroll = ScrollHandle::new();
        self.has_older = false;
        self.issue(Command::SelectChannel(channel));
        cx.notify();
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
                    if self.selected_server.is_none_or(|id| !self.servers.iter().any(|s| s.id == id)) {
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
                UiEvent::Connection(online) => {
                    let was_online = self.connected;
                    self.connected = online;
                    if online && !was_online {
                        if let Some(channel) = self.selected_channel {
                            self.issue(Command::SelectChannel(channel));
                        }
                    }
                }
                UiEvent::Gateway(GatewayMessage::MessageCreated { message }) => self.push_message(message),
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
                        self.issue(Command::SelectChannel(channel));
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
            .size_full().bg(rgb(CONTENT)).font_family("Segoe UI").text_color(rgb(TEXT)).flex().items_center().justify_center()
            .child(
                div().w(px(420.)).bg(rgb(SIDEBAR)).rounded_lg().p_8().flex().flex_col().gap_5()
                    .child(div().text_size(px(27.)).font_weight(FontWeight::BOLD).child("UTAchat"))
                    .child(div().text_size(px(13.)).text_color(rgb(MUTED)).child(if self.register { "Create your UTAchat account" } else { "Sign in to UTAchat" }))
                    .child(div().flex().flex_col().gap_2()
                        .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("USERNAME"))
                        .child(input_frame(&self.username)))
                    .child(div().flex().flex_col().gap_2()
                        .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("PASSWORD"))
                        .child(input_frame(&self.password)))
                    .when_some(self.error.as_ref(), |el, error| el.child(div().text_sm().text_color(rgb(0xf27f7f)).child(error.clone())))
                    .child(div().id("submit-auth").h(px(44.)).flex().items_center().justify_center()
                        .rounded_md().bg(rgb(ACCENT)).cursor_pointer().font_weight(FontWeight::SEMIBOLD)
                        .child(if self.busy { "Connecting" } else { label })
                        .on_click(cx.listener(|this, _, _, cx| this.submit_auth(cx))))
                    .child(div().id("toggle-auth").text_sm().text_color(rgb(0x9da8ff)).cursor_pointer()
                        .child(if self.register { "Already have an account? Sign in" } else { "Need an account? Register" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.register = !this.register;
                            this.error = None;
                            cx.notify();
                        })))
            ).into_any_element()
    }

    fn render_server_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rail = div().w(px(72.)).h_full().bg(rgb(RAIL)).py_3().flex().flex_col().items_center().gap_3()
            .child(div().size(px(46.)).rounded_lg().bg(rgb(ACCENT)).flex().items_center().justify_center()
                .font_weight(FontWeight::BOLD).text_base().child("UTA"))
            .child(div().h(px(2.)).w(px(34.)).bg(rgb(LINE)));
        for server in &self.servers {
            let id = server.id;
            let name = server.name.chars().take(2).collect::<String>().to_uppercase();
            let active = Some(id) == self.selected_server;
            rail = rail.child(
                div().id(format!("server-{id}")).size(px(46.))
                    .rounded_md().flex().items_center().justify_center()
                    .bg(rgb(if active { ACCENT } else { 0x33353e }))
                    .text_color(rgb(TEXT)).font_weight(FontWeight::SEMIBOLD).text_sm()
                    .cursor_pointer().child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_server(id, cx)))
            );
        }
        rail.child(div().id("create-server-trigger").size(px(44.)).rounded_md().bg(rgb(0x2e3038))
            .flex().items_center().justify_center().text_color(rgb(0x58c998)).text_xl().cursor_pointer()
            .child("+").on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateServer; this.error = None; cx.notify(); })))
            .child(div().id("join-server-trigger").size(px(44.)).rounded_md().bg(rgb(0x2e3038))
            .flex().items_center().justify_center().text_color(rgb(MUTED)).text_sm().cursor_pointer()
            .child("JOIN").on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::JoinServer; this.error = None; cx.notify(); })))
            .into_any_element()
    }

    fn render_channel_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let server_name = self.selected_server.and_then(|id| self.servers.iter().find(|s| s.id == id))
            .map(|s| s.name.clone()).unwrap_or_else(|| "No server selected".into());
        let can_create = self.selected_server.and_then(|id| self.servers.iter().find(|s| s.id == id))
            .zip(self.user.as_ref()).is_some_and(|(s, u)| s.owner_id == u.id);
        let mut channel_list = div().id("channel-scroll").flex_1().min_h_0().overflow_y_scrollbar().p_3().flex().flex_col().gap_1()
            .child(div().h(px(30.)).px_2().flex().items_center().justify_between()
                .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("TEXT CHANNELS"))
                .when(can_create, |header| header.child(div().id("create-channel-trigger").cursor_pointer()
                    .text_lg().text_color(rgb(MUTED)).child("+")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateChannel; this.error = None; cx.notify(); })))));
        for channel in &self.channels {
            let id = channel.id;
            let active = Some(id) == self.selected_channel;
            channel_list = channel_list.child(div().id(format!("channel-{id}")).h(px(34.)).px_3().rounded_md()
                .flex().items_center().gap_2().cursor_pointer()
                .bg(rgb(if active { 0x3b3d45 } else { SIDEBAR }))
                .text_color(rgb(if active { TEXT } else { MUTED }))
                .child(div().text_lg().child("#"))
                .child(div().text_sm().font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(channel.name.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.select_channel(id, cx))));
        }
        let username = self.user.as_ref().map(|u| u.username.as_str()).unwrap_or_default();
        div().w(px(248.)).h_full().flex().flex_col().bg(rgb(SIDEBAR))
            .child(div().h(px(56.)).px_4().border_b_1().border_color(rgb(0x18191c))
                .flex().items_center().justify_between()
                .child(div().text_base().font_weight(FontWeight::BOLD).child(server_name))
                .child(div().id("invite-trigger").cursor_pointer().text_xs().text_color(rgb(MUTED))
                    .child("INVITE").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(server) = this.selected_server {
                            this.busy = true;
                            this.issue(Command::FetchInvite(server));
                        }
                        cx.notify();
                    }))))
            .child(channel_list)
            .child(div().h(px(58.)).bg(rgb(0x1c1d21)).px_3().flex().items_center().justify_between()
            .child(div().flex().items_center().gap_2()
                .child(div().size(px(32.)).rounded_full().bg(rgb(0x627d92)).flex().items_center().justify_center()
                    .text_sm().font_weight(FontWeight::BOLD).child(initials(username)))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(username.to_string())))
            .child(div().id("logout-trigger").text_xs().text_color(rgb(MUTED)).cursor_pointer().child("EXIT")
                .on_click(cx.listener(|this, _, _, cx| this.issue(Command::Logout)))))
            .into_any_element()
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let current_channel = self.selected_channel.and_then(|id| self.channels.iter().find(|c| c.id == id));
        let channel_name = current_channel.map(|c| c.name.as_str()).unwrap_or("Select a channel");
        let mut stream = div().id("message-scroll").flex_1().min_h_0().track_scroll(&self.chat_scroll).overflow_y_scrollbar().px_5().py_4().flex().flex_col().gap_1();
        if self.has_older && self.selected_channel.is_some() {
            stream = stream.child(div().id("older-trigger").h(px(32.)).mb_4().flex().items_center().justify_center()
                .text_xs().text_color(rgb(0xaeb6ff)).cursor_pointer().child("Load earlier messages")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let (Some(channel), Some(oldest)) = (this.selected_channel, this.messages.first()) {
                        this.issue(Command::LoadOlder { channel, before: oldest.id });
                    }
                    cx.notify();
                })));
        }
        if self.messages.is_empty() && self.selected_channel.is_some() {
            stream = stream.child(div().flex_1().flex().items_center().justify_center()
                .text_sm().text_color(rgb(MUTED)).child("No messages in this channel"));
        } else {
            for (i, message) in self.messages.iter().enumerate() {
                let continuation = i > 0 && self.messages[i - 1].author.id == message.author.id
                    && (message.created_at - self.messages[i - 1].created_at).num_minutes() < 5;
                stream = stream.child(render_message(message, continuation));
            }
        }
        let mut center = div().flex_1().min_w_0().h_full().flex().flex_col().bg(rgb(CONTENT))
            .child(div().h(px(56.)).bg(rgb(HEADER)).border_b_1().border_color(rgb(0x25262a)).px_5()
                .flex().items_center().justify_between()
                .child(div().flex().items_center().gap_3()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("#"))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(channel_name.to_string())))
                .child(div().flex().items_center().gap_2()
                    .child(div().size(px(7.)).rounded_full().bg(rgb(if self.connected { 0x46b77c } else { 0xdc9e59 })))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(if self.connected { "Connected" } else { "Reconnecting" }))))
            .child(stream);
        if self.selected_channel.is_some() {
            center = center.child(div().px_5().pb_5().pt_2()
                .child(div().h(px(48.)).bg(rgb(INPUT_BG)).rounded_md().px_4().flex().items_center().gap_3()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.composer).appearance(false).bordered(false).w_full()))
                    .child(div().id("send-trigger").px_3().py_2().rounded_md().bg(rgb(ACCENT))
                        .cursor_pointer().text_xs().font_weight(FontWeight::BOLD)
                        .child(if self.sending { "SENDING" } else { "SEND" })
                        .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx))))));
        }
        center.into_any_element()
    }

    fn render_members(&self) -> AnyElement {
        let mut body = div().w(px(222.)).h_full().bg(rgb(MEMBER_BG)).flex().flex_col()
            .child(div().h(px(56.)).px_4().flex().items_center()
                .child(div().text_xs().text_color(rgb(MUTED)).font_weight(FontWeight::BOLD)
                    .child(format!("MEMBERS — {}", self.members.len()))))
            .child(div().h(px(1.)).bg(rgb(LINE)));
        for member in &self.members {
            let name = member.user.username.clone();
            body = body.child(div().h(px(47.)).px_4().flex().items_center().gap_3()
                .child(div().size(px(31.)).rounded_full().bg(rgb(if member.role == "owner" { 0x846b93 } else { 0x477f8b }))
                    .flex().items_center().justify_center().text_sm().font_weight(FontWeight::BOLD).child(initials(&name)))
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_sm().text_color(rgb(TEXT)).child(name))
                    .when(member.role == "owner", |el| el.child(div().text_xs().text_color(rgb(MUTED)).child("OWNER")))));
        }
        body.into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        div().flex_1().bg(rgb(CONTENT)).flex().flex_col().items_center().justify_center().gap_5()
            .child(div().text_size(px(30.)).font_weight(FontWeight::BOLD).child("Make a space of your own"))
            .child(div().text_sm().text_color(rgb(MUTED)).child("Create a server or join one with an invite code."))
            .child(div().flex().gap_3()
                .child(div().id("empty-create").px_5().py_3().rounded_md().bg(rgb(ACCENT)).cursor_pointer().child("Create server")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::CreateServer; cx.notify(); })))
                .child(div().id("empty-join").px_5().py_3().rounded_md().bg(rgb(0x444650)).cursor_pointer().child("Join server")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::JoinServer; cx.notify(); }))))
            .into_any_element()
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let (title, input, action) = match self.modal {
            Modal::CreateServer => ("Create server", Some(&self.server_name), "CREATE"),
            Modal::CreateChannel => ("Create channel", Some(&self.channel_name), "CREATE"),
            Modal::JoinServer => ("Join server", Some(&self.invite_input), "JOIN"),
            Modal::Invite => ("Invite to server", None, "COPY CODE"),
            Modal::None => ("", None, ""),
        };
        let mut card = div().w(px(440.)).bg(rgb(SIDEBAR)).rounded_lg().p_6().flex().flex_col().gap_5()
            .child(div().flex().items_center().justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                .child(div().id("modal-close").text_sm().text_color(rgb(MUTED)).cursor_pointer().child("CLOSE")
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Modal::None; this.error = None; cx.notify(); }))));
        if let Some(input) = input {
            card = card.child(input_frame(input));
        } else {
            card = card.child(div().p_4().rounded_md().bg(rgb(INPUT_BG)).text_sm().child(self.invite_code.clone()));
        }
        if let Some(error) = &self.error {
            card = card.child(div().text_sm().text_color(rgb(0xf27f7f)).child(error.clone()));
        }
        card = card.child(div().id("modal-submit").h(px(43.)).rounded_md().bg(rgb(ACCENT)).flex().items_center()
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
                    Modal::None => {}
                }
                cx.notify();
            })));
        div().absolute().inset_0().bg(rgba(0x000000bd)).flex().items_center().justify_center().child(card).into_any_element()
    }
}

impl Render for UTAchat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.user.is_none() {
            return self.render_auth(cx);
        }
        let mut app = div().relative().size_full().flex().flex_col().bg(rgb(CONTENT)).text_color(rgb(TEXT))
            .font_family("Segoe UI")
            .child(div().flex_1().min_h_0().flex()
                .child(self.render_server_rail(cx))
                .when(self.selected_server.is_some(), |row| row.child(self.render_channel_sidebar(cx)))
                .when(self.selected_server.is_some(), |row| row.child(self.render_chat(cx)))
                .when(self.selected_server.is_some(), |row| row.child(self.render_members()))
                .when(self.selected_server.is_none(), |row| row.child(self.render_empty(cx))));
        if let Some(error) = &self.error {
            app = app.child(div().absolute().top(px(12.)).right(px(16.)).w(px(400.)).p_3()
                .bg(rgb(0x673b43)).rounded_md().flex().items_center().justify_between().gap_3()
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
    div().h(px(44.)).px_3().bg(rgb(INPUT_BG)).rounded_md().flex().items_center()
        .child(Input::new(state).appearance(false).bordered(false).w_full())
        .into_any_element()
}

fn render_message(message: &ChatMessage, continuation: bool) -> AnyElement {
    let avatar = initials(&message.author.username);
    let color = avatar_color(message.author.id);
    div().w_full().px_1().py_1().flex().items_start().gap_3()
        .child(div().w(px(40.)).flex().justify_center()
            .when(!continuation, |el| el.child(div().size(px(36.)).rounded_full().bg(rgb(color))
                .flex().items_center().justify_center().font_weight(FontWeight::BOLD).text_sm().child(avatar))))
        .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
            .when(!continuation, |el| el.child(div().flex().items_center().gap_3()
                .child(div().text_sm().font_weight(FontWeight::BOLD).child(message.author.username.clone()))
                .child(div().text_xs().text_color(rgb(SUBTLE)).child(message.created_at.format("%d %b %Y, %H:%M").to_string()))))
            .child(div().text_sm().text_color(rgb(0xe6e7eb)).child(message.content.clone())))
        .into_any_element()
}

fn initials(name: &str) -> String {
    name.chars().take(2).collect::<String>().to_uppercase()
}

fn avatar_color(user: Uuid) -> u32 {
    const COLORS: [u32; 8] = [0x627d92, 0x85719f, 0x468b82, 0x9b795f, 0x657bb7, 0x996e7b, 0x748856, 0x867f96];
    COLORS[(user.as_u128() % COLORS.len() as u128) as usize]
}

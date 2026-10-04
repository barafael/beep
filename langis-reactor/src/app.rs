use std::rc::Rc;

use langis_core::state::{self, ChatState, ConversationView, MessageView, Screen};
use langis_core::{Backend, Config, ConversationId, Event, QrMatrix};
use windows_reactor::*;
use zeroize::Zeroizing;

#[derive(Clone)]
pub enum Message {
    Backend(Event),
    BackendGone,
    Select(ConversationId),
    DraftChanged(String),
    PasswordChanged(String),
    ConfirmationChanged(String),
    Unlock,
    Send,
    DismissError,
}

pub struct Langis {
    backend: Backend,
    events: async_channel::Receiver<Event>,
    chat: ChatState,
    /// PNG of the current linking QR code; WinUI's `Image` takes encoded data.
    link_qr: Option<EncodedImage>,
    draft: String,
    password: Zeroizing<String>,
    confirmation: Zeroizing<String>,
    composer: ElementRef<TextBox>,
}

impl Langis {
    /// Waits for the next backend event on a worker thread. Re-armed after every event, so
    /// at most one worker is parked on the channel and the UI thread never blocks.
    fn listen(&self, context: &ComponentContext<Self>) {
        let events = self.events.clone();
        _ = context.spawn_background(move |_| match events.recv_blocking() {
            Ok(event) => Message::Backend(event),
            Err(_) => Message::BackendGone,
        });
    }
}

impl Component for Langis {
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let backend = Backend::from_env(Config::default());
        let this = Self {
            events: backend.events(),
            backend,
            chat: ChatState::default(),
            link_qr: None,
            draft: String::new(),
            password: Zeroizing::default(),
            confirmation: Zeroizing::default(),
            composer: ElementRef::new(),
        };
        this.listen(context);
        this
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::Backend(event) => {
                if let Event::LinkQr(qr) = &event {
                    self.link_qr = qr_png(qr);
                }
                self.chat.apply(event);
                self.listen(context);
            }
            Message::BackendGone => {
                self.chat
                    .apply(Event::Error("The Signal backend stopped.".into()));
            }
            Message::Select(id) => {
                if let Some(command) = self.chat.select(&id) {
                    self.backend.send(command);
                    _ = self.composer.request_focus();
                }
            }
            Message::DraftChanged(text) => self.draft = text,
            Message::PasswordChanged(text) => self.password = Zeroizing::new(text),
            Message::ConfirmationChanged(text) => self.confirmation = Zeroizing::new(text),
            Message::Unlock => {
                if let Some(command) = self.chat.unlock(&self.password, &self.confirmation) {
                    self.backend.send(command);
                }
                self.password = Zeroizing::default();
                self.confirmation = Zeroizing::default();
            }
            Message::Send => {
                if let Some(command) = self.chat.compose(&self.draft) {
                    self.backend.send(command);
                    self.draft.clear();
                }
            }
            Message::DismissError => self.chat.dismiss_error(),
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("langis");
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Mica)
                .client_size(1000.0, 680.0),
        );

        let body: View = match self.chat.screen() {
            Screen::Connecting => self.connecting(),
            Screen::Password { new, error } => self.password_form(*new, error.as_deref(), context),
            Screen::Link(_) => self.link(),
            Screen::Chats => self.chats(context),
        };

        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                InfoBar::new()
                    .severity(InfoBarSeverity::Error)
                    .title("Error")
                    .message(self.chat.error().unwrap_or_default())
                    .is_open(self.chat.error().is_some())
                    .is_closable(true)
                    .on_closed(context.message(Message::DismissError)),
                Border::new().grid_row(1).content(body),
            ))
            .into()
    }
}

impl Langis {
    fn connecting(&self) -> View {
        StackPanel::new()
            .spacing(12.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                ProgressRing::new().is_active(true).width(48.0).height(48.0),
                TextBlock::new().text("Connecting to Signal…").opacity(0.7),
            ))
            .into()
    }

    fn password_form(&self, new: bool, error: Option<&str>, context: &ViewContext<Self>) -> View {
        let mut fields = vec![
            KeyedView::new(
                "title",
                TextBlock::new()
                    .text(if new { "Choose a password" } else { "Unlock langis" })
                    .font_size(28.0)
                    .font_weight(FontWeight::SEMI_BOLD),
            ),
            KeyedView::new(
                "hint",
                TextBlock::new()
                    .text(if new {
                        "No system keyring was found, so langis will encrypt your messages with this password. It can't be recovered if you forget it."
                    } else {
                        "Enter the password that protects your messages on this computer."
                    })
                    .text_wrapping(TextWrapping::Wrap)
                    .opacity(0.7),
            ),
            KeyedView::new(
                "password",
                PasswordBox::new()
                    .password(self.password.as_str())
                    .placeholder_text("Password")
                    .on_password_changed(context.callback(|text: Rc<str>| Message::PasswordChanged(text.to_string()))),
            ),
        ];
        if new {
            fields.push(KeyedView::new(
                "confirmation",
                PasswordBox::new()
                    .password(self.confirmation.as_str())
                    .placeholder_text("Repeat password")
                    .on_password_changed(
                        context.callback(|text: Rc<str>| {
                            Message::ConfirmationChanged(text.to_string())
                        }),
                    ),
            ));
        }
        if let Some(error) = error {
            fields.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .foreground(ThemeBrush::SystemCritical),
            ));
        }
        fields.push(KeyedView::new(
            "unlock",
            Button::new()
                .style(ButtonStyle::Accent)
                .on_click(context.message(Message::Unlock))
                .content(if new { "Continue" } else { "Unlock" }),
        ));

        let submit_on_enter = KeyAccelerators::new([KeyAccelerator::new(
            AcceleratorKey::Enter,
            AcceleratorModifiers::None,
            context.message(Message::Unlock),
        )]);
        Grid::new()
            .key_accelerators(submit_on_enter)
            .children((StackPanel::new()
                .spacing(12.0)
                .width(360.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .keyed_children(fields),))
            .into()
    }

    fn link(&self) -> View {
        let qr: View = match &self.link_qr {
            Some(png) => Image::new()
                .source_data(png.clone())
                .stretch(Stretch::Uniform)
                .width(256.0)
                .height(256.0)
                .into(),
            None => ProgressRing::new().is_active(true).into(),
        };
        StackPanel::new()
            .spacing(20.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                TextBlock::new()
                    .text("Link langis to your phone")
                    .font_size(28.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .horizontal_alignment(HorizontalAlignment::Center),
                Border::new()
                    // The QR code must stay dark-on-light even in dark mode.
                    .background(Color::rgb(255, 255, 255))
                    .corner_radius(8.0)
                    .padding(12.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .content(qr),
                TextBlock::new()
                    .text("On your phone, open Signal → Settings → Linked devices → Link new device, then scan this code.")
                    .text_wrapping(TextWrapping::Wrap)
                    .max_width(420.0)
                    .opacity(0.7)
                    .horizontal_alignment(HorizontalAlignment::Center),
            ))
            .into()
    }

    fn chats(&self, context: &ViewContext<Self>) -> View {
        let conversation: View = match self.chat.selected() {
            None => TextBlock::new()
                .text("Select a chat to start messaging")
                .opacity(0.6)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
            Some(selected) => self.conversation(&selected.title, context),
        };

        Grid::new()
            .columns([GridLength::Pixel(300.0), GridLength::STAR])
            .children((
                self.sidebar(context),
                Border::new()
                    .grid_column(1)
                    .background(ThemeBrush::SolidBackground)
                    .border_brush(ThemeBrush::CardStroke)
                    .border_thickness(Thickness::new(1.0, 0.0, 0.0, 0.0))
                    .content(conversation),
            ))
            .into()
    }

    fn sidebar(&self, context: &ViewContext<Self>) -> Grid {
        let mut header = vec![KeyedView::new(
            "title",
            TextBlock::new()
                .text("Chats")
                .font_size(20.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center),
        )];
        if !self.chat.synced() {
            header.push(KeyedView::new(
                "sync",
                ProgressRing::new()
                    .is_active(true)
                    .width(16.0)
                    .height(16.0)
                    .vertical_alignment(VerticalAlignment::Center),
            ));
        }

        let rows = self.chat.conversations().map(|view| {
            KeyedView::new(
                view.conversation.id.0.clone(),
                conversation_row(&view, context),
            )
        });

        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .children((
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(10.0)
                    .margin(Thickness::new(16.0, 16.0, 16.0, 8.0))
                    .keyed_children(header),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(2.0)
                        .margin(Thickness::xy(6.0, 0.0))
                        .keyed_children(rows),
                ),
                TextBlock::new()
                    .grid_row(2)
                    .text(self.chat.account())
                    .font_size(11.0)
                    .opacity(0.5)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(8.0),
            ))
    }

    fn conversation(&self, title: &str, context: &ViewContext<Self>) -> View {
        let bubbles = self
            .chat
            .messages()
            .enumerate()
            .map(|(i, view)| KeyedView::new(i, bubble(&view)));

        let can_send = self.chat.can_compose(&self.draft);
        let send_on_enter = KeyAccelerators::new([KeyAccelerator::new(
            AcceleratorKey::Enter,
            AcceleratorModifiers::None,
            context.message(Message::Send),
        )]);

        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .children((
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(12.0)
                    .margin(Thickness::xy(16.0, 12.0))
                    .children((
                        PersonPicture::new()
                            .display_name(title)
                            .width(36.0)
                            .height(36.0),
                        TextBlock::new()
                            .text(title)
                            .font_size(16.0)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
                // TODO: reactor 0.100 exposes no scroll-to API, so new messages don't auto-scroll yet.
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(4.0)
                        .margin(Thickness::xy(16.0, 8.0))
                        .keyed_children(bubbles),
                ),
                Grid::new()
                    .grid_row(2)
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .margin(12.0)
                    .key_accelerators(send_on_enter)
                    .children((
                        TextBox::new(&self.draft)
                            .element_ref(&self.composer)
                            .placeholder_text(if self.chat.synced() {
                                "Message"
                            } else {
                                "Syncing…"
                            })
                            .is_enabled(self.chat.synced())
                            .on_text_changed(
                                context.callback(|text: Rc<str>| {
                                    Message::DraftChanged(text.to_string())
                                }),
                            ),
                        Button::new()
                            .grid_column(1)
                            .style(ButtonStyle::Accent)
                            .is_enabled(can_send)
                            .on_click(context.message(Message::Send))
                            .content("Send"),
                    )),
            ))
            .into()
    }
}

fn conversation_row(view: &ConversationView, context: &ViewContext<Langis>) -> Button {
    let conversation = view.conversation;
    let mut cells = vec![
        KeyedView::new(
            "avatar",
            PersonPicture::new()
                .display_name(&conversation.title)
                .initials(state::initials(&conversation.title))
                .width(40.0)
                .height(40.0),
        ),
        KeyedView::new(
            "text",
            StackPanel::new()
                .grid_column(1)
                .spacing(2.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text(&conversation.title)
                        .font_weight(if view.unread > 0 {
                            FontWeight::BOLD
                        } else {
                            FontWeight::SEMI_BOLD
                        })
                        .text_trimming(TextTrimming::CharacterEllipsis),
                    TextBlock::new()
                        .text(view.preview())
                        .font_size(12.0)
                        .opacity(0.65)
                        .text_trimming(TextTrimming::CharacterEllipsis),
                )),
        ),
    ];
    if view.unread > 0 {
        cells.push(KeyedView::new(
            "badge",
            Border::new()
                .grid_column(2)
                .background(ThemeBrush::Accent)
                .corner_radius(10.0)
                .padding(Thickness::xy(7.0, 1.0))
                .vertical_alignment(VerticalAlignment::Center)
                .content(
                    TextBlock::new()
                        .text(view.unread.to_string())
                        .font_size(11.0)
                        .font_weight(FontWeight::BOLD)
                        .foreground(Color::rgb(255, 255, 255)),
                ),
        ));
    }

    let mut row = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .on_click(context.message(Message::Select(conversation.id.clone())))
        .content(
            Grid::new()
                .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .column_spacing(12.0)
                .margin(Thickness::xy(4.0, 6.0))
                .keyed_children(cells),
        );
    if view.selected {
        row = row.background(ThemeBrush::CardBackground);
    }
    row
}

fn bubble(view: &MessageView) -> View {
    let message = view.message;
    let outgoing = message.outgoing;
    let text = |t: TextBlock| {
        if outgoing {
            t.foreground(Color::rgb(255, 255, 255))
        } else {
            t
        }
    };

    let mut bubble = Border::new()
        .corner_radius(14.0)
        .padding(Thickness::new(12.0, 7.0, 12.0, 6.0))
        .max_width(520.0)
        .horizontal_alignment(if outgoing {
            HorizontalAlignment::Right
        } else {
            HorizontalAlignment::Left
        })
        .content(
            StackPanel::new().spacing(2.0).children((
                text(
                    TextBlock::new()
                        .text(&message.body)
                        .text_wrapping(TextWrapping::Wrap)
                        .is_text_selection_enabled(true),
                ),
                text(
                    TextBlock::new()
                        .text(state::format_time(message.timestamp))
                        .font_size(10.0)
                        .opacity(0.7)
                        .horizontal_alignment(HorizontalAlignment::Right),
                ),
            )),
        );
    bubble = if outgoing {
        bubble.background(ThemeBrush::Accent)
    } else {
        bubble
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
    };

    if !view.show_sender {
        return bubble.into();
    }
    StackPanel::new()
        .spacing(2.0)
        .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        .children((
            TextBlock::new()
                .text(&message.sender)
                .font_size(12.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .opacity(0.7),
            bubble,
        ))
        .into()
}

fn qr_png(qr: &QrMatrix) -> Option<EncodedImage> {
    let (side, rgba) = qr.to_rgba(8);
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, side as u32, side as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().ok()?.write_image_data(&rgba).ok()?;
    }
    Some(EncodedImage::new(png))
}

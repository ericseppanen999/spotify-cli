use crate::{
    api::{Api, CoverPixels, Device, Playback, Playlist, Track},
    config::{Config, Pin, PinKind, Theme},
    newmusic,
};
use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const BINDINGS: &[(&str, &str, &str)] = &[
    ("space", "toggle", "Play / pause"),
    ("n", "next", "Next track"),
    ("p", "previous", "Previous track"),
    ("P", "pin", "Pin / unpin selected song or playlist"),
    ("j", "down", "Move down"),
    ("k", "up", "Move up"),
    ("enter", "play", "Open playlist / play track"),
    ("tab", "focus", "Switch panel"),
    ("/", "search", "Search tracks"),
    ("ctrl+p", "picker", "Find playlist or loaded track"),
    ("a", "add", "Add track to playlist"),
    (
        "m",
        "add_current",
        "Add playing song to this week's New Music",
    ),
    ("l", "like", "Like selected track"),
    ("d", "devices", "Choose device"),
    ("t", "theme", "Choose color theme"),
    ("c", "create", "Create new playlist"),
    ("+", "louder", "Volume up"),
    ("-", "quieter", "Volume down"),
    ("N", "newmusic", "Toggle new music mode"),
    ("x", "remove", "Remove selected track"),
    ("r", "recent", "Recently played"),
    ("z", "listening", "Listening mode (also after 60s idle)"),
    ("?", "help", "Help"),
    ("q", "quit", "Quit"),
];
#[derive(Clone)]
enum Command {
    Load(String, String),
    Search(String),
    Recent,
    Toggle,
    Next,
    Previous,
    Play(String),
    Devices,
    Transfer(String),
    Volume(i16),
    Like(String),
    Add(String, Option<String>),
    AddCurrent,
    Remove(String, String),
    Newmusic,
    Pin(Pin),
    Theme(Theme),
    NewPlaylist(String),
}
enum Update {
    Busy(bool),
    Playlists(Vec<Playlist>),
    Tracks(Vec<Track>, String, Option<String>),
    Playback(Box<Playback>),
    Devices(Vec<Device>),
    Message(String, bool),
    Newmusic(bool),
    Pins(Vec<Pin>),
    Theme(Theme),
    Cover(Option<CoverPixels>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Pins,
    Playlists,
    Tracks,
}
enum Popup {
    Help,
    Search,
    Picker,
    Add(String),
    Devices,
    Theme,
    NewPlaylist,
    Remove(String, String),
}
struct Ui {
    playlists: Vec<Playlist>,
    tracks: Vec<Track>,
    devices: Vec<Device>,
    playback: Playback,
    playlist_state: ListState,
    track_state: ListState,
    popup_state: ListState,
    pin_state: ListState,
    focus: Panel,
    popup: Option<Popup>,
    input: String,
    title: String,
    playlist: Option<String>,
    message: String,
    error: bool,
    message_at: Instant,
    playback_at: Instant,
    busy: bool,
    tick: usize,
    config: Config,
    last_activity: Instant,
    listening: bool,
    cover: Option<CoverPixels>,
}
impl Ui {
    fn new(config: Config) -> Self {
        Self {
            playlists: vec![],
            tracks: vec![],
            devices: vec![],
            playback: Playback::default(),
            playlist_state: ListState::default().with_selected(Some(0)),
            track_state: ListState::default().with_selected(Some(0)),
            popup_state: ListState::default().with_selected(Some(0)),
            pin_state: ListState::default().with_selected(Some(0)),
            focus: if config.pins.is_empty() {
                Panel::Playlists
            } else {
                Panel::Pins
            },
            popup: None,
            input: String::new(),
            title: "Choose a playlist and press Enter".into(),
            playlist: None,
            message: "Connecting to Spotify...".into(),
            error: false,
            message_at: Instant::now(),
            playback_at: Instant::now(),
            busy: true,
            tick: 0,
            config,
            last_activity: Instant::now(),
            listening: false,
            cover: None,
        }
    }
    fn notice(&mut self, message: String, error: bool) {
        self.message = message;
        self.error = error;
        self.message_at = Instant::now();
    }
    fn update(&mut self, update: Update) {
        match update {
            Update::Busy(b) => self.busy = b,
            Update::Playlists(p) => self.playlists = p,
            Update::Tracks(t, title, id) => {
                self.tracks = t;
                self.title = title;
                self.playlist = id;
                self.track_state.select(Some(0));
                self.focus = Panel::Tracks;
            }
            Update::Playback(p) => {
                self.playback = *p;
                self.playback_at = Instant::now();
            }
            Update::Devices(d) => {
                self.devices = d;
                self.open(Popup::Devices);
            }
            Update::Message(m, e) => self.notice(m, e),
            Update::Newmusic(on) => self.config.newmusic = on,
            Update::Theme(theme) => self.config.theme = theme,
            Update::Cover(c) => self.cover = c,
            Update::Pins(pins) => {
                self.config.pins = pins;
                self.pin_state.select(if self.config.pins.is_empty() {
                    None
                } else {
                    Some(
                        self.pin_state
                            .selected()
                            .unwrap_or(0)
                            .min(self.config.pins.len() - 1),
                    )
                });
            }
        }
    }
    fn open(&mut self, popup: Popup) {
        self.popup = Some(popup);
        self.input.clear();
        self.popup_state.select(Some(0));
    }
    fn selected_track(&self) -> Option<String> {
        match self.focus {
            Panel::Pins => self
                .config
                .pins
                .get(self.pin_state.selected().unwrap_or(0))
                .filter(|p| p.kind == PinKind::Track)
                .map(|p| p.uri.clone()),
            Panel::Playlists => None,
            Panel::Tracks => self
                .tracks
                .get(self.track_state.selected().unwrap_or(0))
                .map(|t| t.uri.clone()),
        }
    }
    fn selected_pin(&self) -> Option<Pin> {
        match self.focus {
            Panel::Pins => self
                .config
                .pins
                .get(self.pin_state.selected().unwrap_or(0))
                .cloned(),
            Panel::Playlists => self
                .playlists
                .get(self.playlist_state.selected().unwrap_or(0))
                .map(|p| Pin {
                    kind: PinKind::Playlist,
                    uri: format!("spotify:playlist:{}", p.id),
                    name: p.name.clone(),
                }),
            Panel::Tracks => self
                .tracks
                .get(self.track_state.selected().unwrap_or(0))
                .filter(|t| t.uri.starts_with("spotify:track:"))
                .map(|t| Pin {
                    kind: PinKind::Track,
                    uri: t.uri.clone(),
                    name: t.label(),
                }),
        }
    }
    fn choices(&self) -> Vec<(String, Command)> {
        let choices: Vec<_> = match &self.popup {
            Some(Popup::Devices) => self
                .devices
                .iter()
                .filter(|d| !d.is_restricted)
                .filter_map(|d| {
                    d.id.as_ref()
                        .map(|id| (d.name.clone(), Command::Transfer(id.clone())))
                })
                .collect(),
            Some(Popup::Add(track)) => self
                .playlists
                .iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        Command::Add(track.clone(), Some(p.id.clone())),
                    )
                })
                .collect(),
            Some(Popup::Picker) => self
                .playlists
                .iter()
                .map(|p| {
                    (
                        format!("Playlist: {}", p.name),
                        Command::Load(p.id.clone(), p.name.clone()),
                    )
                })
                .chain(
                    self.tracks
                        .iter()
                        .map(|t| (t.label(), Command::Play(t.uri.clone()))),
                )
                .collect(),
            Some(Popup::Theme) => Theme::presets()
                .into_iter()
                .map(|(name, theme)| (name.to_string(), Command::Theme(theme)))
                .collect(),
            _ => vec![],
        };
        choices
            .into_iter()
            .filter(|(name, _)| fuzzy(&self.input, name))
            .collect()
    }
    fn key(&mut self, key: KeyEvent, tx: &mpsc::Sender<Command>) -> bool {
        self.last_activity = Instant::now();
        if self.listening {
            self.listening = false;
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.popup.is_some() {
            match key.code {
                KeyCode::Esc => self.popup = None,
                KeyCode::Enter => {
                    let command = match &self.popup {
                        Some(Popup::Search) if !self.input.trim().is_empty() => {
                            Some(Command::Search(self.input.clone()))
                        }
                        Some(Popup::NewPlaylist) if !self.input.trim().is_empty() => {
                            Some(Command::NewPlaylist(self.input.trim().to_string()))
                        }
                        Some(Popup::Remove(track, playlist)) if self.input == "yes" => {
                            Some(Command::Remove(track.clone(), playlist.clone()))
                        }
                        Some(Popup::Picker | Popup::Add(_) | Popup::Devices | Popup::Theme) => self
                            .choices()
                            .get(self.popup_state.selected().unwrap_or(0))
                            .map(|(_, c)| c.clone()),
                        _ => None,
                    };
                    if let Some(command) = command {
                        self.send(tx, command);
                        self.popup = None;
                    } else if matches!(self.popup, Some(Popup::Help)) {
                        self.popup = None;
                    }
                }
                KeyCode::Down => {
                    let len = if matches!(self.popup, Some(Popup::Help)) {
                        BINDINGS.len() + 1
                    } else {
                        self.choices().len()
                    };
                    move_selection(&mut self.popup_state, len, 1);
                }
                KeyCode::Up => {
                    let len = if matches!(self.popup, Some(Popup::Help)) {
                        BINDINGS.len() + 1
                    } else {
                        self.choices().len()
                    };
                    move_selection(&mut self.popup_state, len, -1);
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    self.popup_state.select(Some(0));
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.push(c);
                    self.popup_state.select(Some(0));
                }
                _ => {}
            }
            return false;
        }
        let name = key_name(key);
        let action = self
            .config
            .keys
            .get(&name)
            .map(String::as_str)
            .or_else(|| {
                BINDINGS
                    .iter()
                    .find(|(k, _, _)| *k == name)
                    .map(|(_, a, _)| *a)
            })
            .or(match key.code {
                KeyCode::Down => Some("down"),
                KeyCode::Up => Some("up"),
                _ => None,
            })
            .unwrap_or("")
            .to_string();
        let command = match action.as_str() {
            "listening" => {
                self.listening = true;
                None
            }
            "quit" => return true,
            "focus" => {
                self.focus = match self.focus {
                    Panel::Pins => Panel::Playlists,
                    Panel::Playlists => Panel::Tracks,
                    Panel::Tracks => Panel::Pins,
                };
                None
            }
            "down" | "up" => {
                let delta = if action == "down" { 1 } else { -1 };
                match self.focus {
                    Panel::Tracks => {
                        move_selection(&mut self.track_state, self.tracks.len(), delta)
                    }
                    Panel::Playlists => {
                        move_selection(&mut self.playlist_state, self.playlists.len(), delta)
                    }
                    Panel::Pins => {
                        move_selection(&mut self.pin_state, self.config.pins.len(), delta)
                    }
                }
                None
            }
            "pin" => self.selected_pin().map(Command::Pin),
            "play" if self.focus == Panel::Pins => self.selected_pin().map(|p| match p.kind {
                PinKind::Track => Command::Play(p.uri),
                PinKind::Playlist => Command::Load(p.uri, p.name),
            }),
            "play" if self.focus == Panel::Playlists => self
                .playlists
                .get(self.playlist_state.selected().unwrap_or(0))
                .map(|p| Command::Load(p.id.clone(), p.name.clone())),
            "play" => self.selected_track().map(Command::Play),
            "toggle" => Some(Command::Toggle),
            "next" => Some(Command::Next),
            "previous" => Some(Command::Previous),
            "search" => {
                self.open(Popup::Search);
                None
            }
            "picker" => {
                self.open(Popup::Picker);
                None
            }
            "help" => {
                self.open(Popup::Help);
                None
            }
            "devices" => Some(Command::Devices),
            "theme" => {
                self.open(Popup::Theme);
                None
            }
            "create" => {
                self.open(Popup::NewPlaylist);
                None
            }
            "add_current" => Some(Command::AddCurrent),
            "like" => self
                .selected_track()
                .or_else(|| self.playback.item.as_ref().map(|t| t.uri.clone()))
                .map(Command::Like),
            "add" => {
                if let Some(track) = self.selected_track() {
                    if self.config.newmusic {
                        Some(Command::Add(track, None))
                    } else {
                        self.open(Popup::Add(track));
                        None
                    }
                } else {
                    None
                }
            }
            "remove" if self.focus == Panel::Tracks => {
                if let (Some(track), Some(playlist)) =
                    (self.selected_track(), self.playlist.clone())
                {
                    self.open(Popup::Remove(track, playlist));
                }
                None
            }
            "louder" => Some(Command::Volume(5)),
            "quieter" => Some(Command::Volume(-5)),
            "newmusic" => Some(Command::Newmusic),
            "recent" => Some(Command::Recent),
            _ => None,
        };
        if let Some(c) = command {
            self.send(tx, c);
        }
        false
    }
    fn send(&mut self, tx: &mpsc::Sender<Command>, command: Command) {
        if tx.try_send(command).is_err() {
            self.notice("Please wait for the current request".into(), true);
        } else {
            self.busy = true;
        }
    }
    fn tick(&mut self, now: Instant) {
        self.tick = self.tick.wrapping_add(1);
        if now.saturating_duration_since(self.last_activity) >= Duration::from_secs(60) {
            self.listening = true;
        }
    }
}
fn key_name(key: KeyEvent) -> String {
    let value = match key.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        _ => String::new(),
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        format!("ctrl+{value}")
    } else {
        value
    }
}
fn move_selection(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
    } else {
        state.select(Some(
            (state.selected().unwrap_or(0) as isize + delta).rem_euclid(len as isize) as usize,
        ));
    }
}
fn fuzzy(query: &str, text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut chars = lower.chars();
    query
        .to_lowercase()
        .chars()
        .all(|wanted| chars.any(|c| c == wanted))
}
fn color(value: &str, fallback: Color) -> Color {
    value.parse().unwrap_or(fallback)
}
fn draw(frame: &mut Frame, ui: &mut Ui) {
    if ui.listening {
        draw_listening(frame, ui);
        return;
    }
    let accent = color(&ui.config.theme.accent, Color::Green);
    let dim = color(&ui.config.theme.dim, Color::DarkGray);
    let highlight = color(&ui.config.theme.highlight, Color::DarkGray);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.tick % 10];
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" SPOT ", Style::default().fg(accent)),
            Span::raw(if ui.config.newmusic {
                "  NEW MUSIC ON"
            } else {
                "  YOUR MUSIC"
            }),
            Span::styled(
                format!("   {}", if ui.busy { spinner } else { "" }),
                Style::default().fg(accent),
            ),
        ])),
        rows[0],
    );
    let panels =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).split(rows[1]);
    let border = |title: String, focused: bool| {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(if focused { accent } else { dim }))
    };
    let playlists = List::new(
        ui.playlists
            .iter()
            .map(|p| ListItem::new(p.name.clone()))
            .collect::<Vec<_>>(),
    )
    .block(border(" Playlists ".into(), ui.focus == Panel::Playlists))
    .highlight_style(Style::default().bg(highlight))
    .highlight_symbol("› ");
    let pin_height = (ui.config.pins.len().clamp(1, 6) as u16 + 2).min(panels[0].height / 2);
    let sidebar =
        Layout::vertical([Constraint::Length(pin_height), Constraint::Min(0)]).split(panels[0]);
    let pin_items = if ui.config.pins.is_empty() {
        vec![ListItem::new("Select an item · Shift+P").style(Style::default().fg(dim))]
    } else {
        ui.config
            .pins
            .iter()
            .map(|p| {
                ListItem::new(format!(
                    "{} {}",
                    if p.kind == PinKind::Track {
                        "♪"
                    } else {
                        "≡"
                    },
                    p.name
                ))
            })
            .collect()
    };
    let pins = List::new(pin_items)
        .block(border(" Pins · P ".into(), ui.focus == Panel::Pins))
        .highlight_style(Style::default().bg(highlight))
        .highlight_symbol("› ");
    frame.render_stateful_widget(pins, sidebar[0], &mut ui.pin_state);
    frame.render_stateful_widget(playlists, sidebar[1], &mut ui.playlist_state);
    let tracks = List::new(
        ui.tracks
            .iter()
            .map(|t| {
                ListItem::new(vec![
                    Line::from(t.label()),
                    Line::from(Span::styled(
                        format!(
                            "  {}  {}:{:02}",
                            t.album.name,
                            t.duration_ms / 60000,
                            (t.duration_ms / 1000) % 60
                        ),
                        Style::default().fg(dim),
                    )),
                ])
            })
            .collect::<Vec<_>>(),
    )
    .block(border(format!(" {} ", ui.title), ui.focus == Panel::Tracks))
    .highlight_style(Style::default().bg(highlight))
    .highlight_symbol("› ");
    frame.render_stateful_widget(tracks, panels[1], &mut ui.track_state);
    let now = ui
        .playback
        .item
        .as_ref()
        .map(|t| t.label())
        .unwrap_or_else(|| {
            "Nothing playing — open Spotify on a device; press d to choose it".into()
        });
    let device = ui
        .playback
        .device
        .as_ref()
        .map(|d| d.name.as_str())
        .unwrap_or("No device");
    let playback = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .title(format!(
            " {} · {} ",
            if ui.playback.is_playing {
                "Playing"
            } else {
                "Paused"
            },
            device
        ));
    let inner = playback.inner(rows[2]);
    frame.render_widget(playback, rows[2]);
    let now_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
    frame.render_widget(Paragraph::new(now), now_rows[0]);
    let (position, duration, ratio) = progress(ui);
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(accent))
            .label(format!(
                "{}:{:02} / {}:{:02}",
                position / 60000,
                position / 1000 % 60,
                duration / 60000,
                duration / 1000 % 60
            )),
        now_rows[1],
    );
    let footer = if ui.message_at.elapsed() < Duration::from_secs(8) {
        ui.message.as_str()
    } else {
        " ? help   / search   a add   d devices   N new music   q quit"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(
            if ui.error && ui.message_at.elapsed() < Duration::from_secs(8) {
                color(&ui.config.theme.error, Color::Red)
            } else {
                dim
            },
        )),
        rows[3],
    );
    if let Some(popup) = &ui.popup {
        let area = centered(frame.area());
        frame.render_widget(Clear, area);
        let title = match popup {
            Popup::Help => " Keyboard shortcuts · Esc closes ",
            Popup::Search => " Search tracks · Enter searches ",
            Popup::Picker => " Jump to playlist / loaded track ",
            Popup::Add(_) => " Add to playlist ",
            Popup::Devices => " Send playback to device ",
            Popup::Theme => " Choose a theme ",
            Popup::NewPlaylist => " Create new playlist · Enter creates ",
            Popup::Remove(_, _) => " Remove track (all occurrences) · type yes, Enter ",
        };
        let block = border(title.into(), true);
        let inside = block.inner(area);
        frame.render_widget(block, area);
        if matches!(popup, Popup::Help) {
            let mut lines: Vec<Line> = BINDINGS
                .iter()
                .map(|(k, a, d)| {
                    let mut keys = ui
                        .config
                        .keys
                        .iter()
                        .filter(|(_, v)| v.as_str() == *a)
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>();
                    if !ui.config.keys.contains_key(*k) {
                        keys.insert(0, k);
                    }
                    let remapped = keys.join(", ");
                    Line::from(format!(
                        "{:<12} {}",
                        if remapped.is_empty() {
                            "(unbound)"
                        } else {
                            &remapped
                        },
                        d
                    ))
                })
                .collect();
            lines.push(Line::from("Esc closes popups · Ctrl+C quits"));
            frame.render_widget(
                Paragraph::new(lines).scroll((ui.popup_state.selected().unwrap_or(0) as u16, 0)),
                inside,
            );
        } else {
            let areas = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inside);
            frame.render_widget(Paragraph::new(format!("> {}▏", ui.input)), areas[0]);
            let choices = ui.choices();
            let list = List::new(
                choices
                    .into_iter()
                    .map(|(label, _)| ListItem::new(label))
                    .collect::<Vec<_>>(),
            )
            .highlight_style(Style::default().bg(highlight))
            .highlight_symbol("› ");
            frame.render_stateful_widget(list, areas[1], &mut ui.popup_state);
        }
    }
}
fn progress(ui: &Ui) -> (u64, u64, f64) {
    let duration = ui
        .playback
        .item
        .as_ref()
        .map(|t| t.duration_ms)
        .unwrap_or(0);
    let elapsed = if ui.playback.is_playing {
        ui.playback_at.elapsed().as_millis() as u64
    } else {
        0
    };
    let position = ui
        .playback
        .progress_ms
        .unwrap_or(0)
        .saturating_add(elapsed)
        .min(duration);
    (
        position,
        duration,
        if duration == 0 {
            0.0
        } else {
            position as f64 / duration as f64
        },
    )
}
fn draw_listening(frame: &mut Frame, ui: &Ui) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(80);
    let text_height = 12;
    let art_width = ui.cover.as_ref().map(|_| width.min(28)).unwrap_or(0);
    let art_height = art_width / 2;
    let height = (art_height + text_height).min(area.height);
    let center = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let mut constraints = Vec::new();
    if art_height > 0 {
        constraints.push(Constraint::Length(art_height));
    }
    constraints.extend([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
    ]);
    let rows = Layout::vertical(constraints).split(center);
    let offset = if art_height > 0 { 1 } else { 0 };
    if let Some(cover) = ui.cover.as_ref().filter(|_| art_height > 0) {
        let art_area = Rect::new(
            center.x + (width.saturating_sub(art_width)) / 2,
            rows[0].y,
            art_width,
            art_height,
        );
        render_cover(frame, art_area, cover);
    }
    let rows = &rows[offset..];
    let accent = color(&ui.config.theme.accent, Color::Green);
    let dim = color(&ui.config.theme.dim, Color::DarkGray);
    let track = ui.playback.item.as_ref();
    let title = track.map(|t| t.name.as_str()).unwrap_or("Nothing playing");
    let artists = track
        .map(|t| {
            t.artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new("L I S T E N I N G")
            .alignment(Alignment::Center)
            .style(Style::default().fg(accent)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(title)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::default().add_modifier(Modifier::BOLD)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(artists)
            .alignment(Alignment::Center)
            .style(Style::default().fg(dim)),
        rows[2],
    );
    let (position, duration, ratio) = progress(ui);
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(accent))
            .label(format!(
                "{}:{:02} / {}:{:02}",
                position / 60000,
                position / 1000 % 60,
                duration / 60000,
                duration / 1000 % 60
            )),
        rows[3],
    );
    let status = format!(
        "{} · {}",
        if ui.playback.is_playing {
            "Playing"
        } else {
            "Paused"
        },
        ui.playback
            .device
            .as_ref()
            .map(|d| d.name.as_str())
            .unwrap_or("No active device")
    );
    frame.render_widget(
        Paragraph::new(status)
            .alignment(Alignment::Center)
            .style(Style::default().fg(dim)),
        rows[4],
    );
    frame.render_widget(
        Paragraph::new("Press any key to return")
            .alignment(Alignment::Center)
            .style(Style::default().fg(dim)),
        rows[5],
    );
}
/// Renders an RGB pixel grid as half-block characters, doubling vertical resolution
/// by pairing each cell's foreground/background color with one pixel each.
fn render_cover(frame: &mut Frame, area: Rect, cover: &CoverPixels) {
    let size = cover.len() as u32;
    if size == 0 || area.width == 0 || area.height == 0 {
        return;
    }
    let pixel_width = area.width as u32;
    let pixel_height = area.height as u32 * 2;
    let sample = |x: u32, y: u32| {
        let sx = (x * size / pixel_width).min(size - 1) as usize;
        let sy = (y * size / pixel_height).min(size - 1) as usize;
        cover[sy][sx]
    };
    let lines: Vec<Line> = (0..area.height as u32)
        .map(|row| {
            let spans: Vec<Span> = (0..pixel_width)
                .map(|col| {
                    let (tr, tg, tb) = sample(col, row * 2);
                    let (br, bg, bb) = sample(col, row * 2 + 1);
                    Span::styled(
                        "▀",
                        Style::default()
                            .fg(Color::Rgb(tr, tg, tb))
                            .bg(Color::Rgb(br, bg, bb)),
                    )
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}
fn centered(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(85);
    let height = area.height.saturating_sub(2).min(26);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn emit(tx: &mpsc::UnboundedSender<Update>, update: Update) {
    let _ = tx.send(update);
}
async fn execute(
    command: Command,
    api: &mut Api,
    config: &mut Config,
    dir: &std::path::Path,
    tx: &mpsc::UnboundedSender<Update>,
) -> Result<()> {
    match command {
        Command::Pin(pin) => {
            let mut updated = config.clone();
            let added = updated.toggle_pin(pin);
            updated.save(dir)?;
            *config = updated;
            emit(tx, Update::Pins(config.pins.clone()));
            emit(
                tx,
                Update::Message(if added { "Pinned" } else { "Unpinned" }.into(), false),
            );
        }
        Command::Theme(theme) => {
            config.theme = theme;
            config.save(dir)?;
            emit(tx, Update::Theme(config.theme.clone()));
            emit(tx, Update::Message("Theme updated".into(), false));
        }
        Command::NewPlaylist(name) => {
            api.create_playlist(&name, "").await?;
            emit(
                tx,
                Update::Message(format!("Created playlist {name}"), false),
            );
            emit(tx, Update::Playlists(api.playlists().await?));
        }
        Command::Load(id, name) => {
            let tracks = api.playlist_tracks(&id).await?;
            emit(tx, Update::Tracks(tracks, name, Some(id)));
        }
        Command::Search(q) => {
            let tracks = api.search_tracks(&q).await?;
            let title = format!("Search: {q} ({} results, up to 50)", tracks.len());
            emit(tx, Update::Tracks(tracks, title, None));
        }
        Command::Recent => emit(
            tx,
            Update::Tracks(api.recent().await?, "Recently played".into(), None),
        ),
        Command::Devices => emit(tx, Update::Devices(api.devices().await?)),
        Command::Toggle => {
            if api.playback().await?.is_playing {
                api.pause().await?;
            } else {
                api.play(None).await?;
            }
        }
        Command::Next => api.next().await?,
        Command::Previous => api.previous().await?,
        Command::Play(uri) => api.play(Some(&uri)).await?,
        Command::Transfer(id) => api.transfer(&id).await?,
        Command::Volume(delta) => {
            let current = api
                .playback()
                .await?
                .device
                .and_then(|d| d.volume_percent)
                .context("This device does not expose volume control")?;
            api.volume((i16::from(current) + delta).clamp(0, 100) as u8)
                .await?;
        }
        Command::Like(uri) => {
            api.like(&uri).await?;
            emit(tx, Update::Message("Saved to your library".into(), false));
        }
        Command::Add(uri, playlist) => {
            let id = match playlist {
                Some(id) => id,
                None => newmusic::ensure(api, config, dir).await?,
            };
            api.add(&uri, &id).await?;
            let name = if config.newmusic && config.weeks.values().any(|v| v == &id) {
                newmusic::playlist_name(newmusic::release_day(chrono::Utc::now()))
            } else {
                "playlist".into()
            };
            emit(tx, Update::Message(format!("Added to {name}"), false));
            emit(tx, Update::Playlists(api.playlists().await?));
        }
        Command::AddCurrent => {
            newmusic::add_current(api, config, dir).await?;
            emit(
                tx,
                Update::Message(
                    format!(
                        "Added playing song to {}",
                        newmusic::playlist_name(newmusic::release_day(chrono::Utc::now()))
                    ),
                    false,
                ),
            );
            emit(tx, Update::Playlists(api.playlists().await?));
        }
        Command::Remove(uri, playlist) => {
            api.remove(&uri, &playlist).await?;
            emit(
                tx,
                Update::Tracks(
                    api.playlist_tracks(&playlist).await?,
                    "Playlist".into(),
                    Some(playlist),
                ),
            );
            emit(tx, Update::Message("Removed from playlist".into(), false));
        }
        Command::Newmusic => {
            if !config.newmusic {
                newmusic::ensure(api, config, dir).await?;
            }
            config.newmusic = !config.newmusic;
            config.save(dir)?;
            emit(tx, Update::Newmusic(config.newmusic));
            emit(tx, Update::Playlists(api.playlists().await?));
            emit(
                tx,
                Update::Message(
                    format!(
                        "New music mode: {}",
                        if config.newmusic { "on" } else { "off" }
                    ),
                    false,
                ),
            );
        }
    }
    Ok(())
}
async fn worker(
    mut api: Api,
    mut config: Config,
    dir: PathBuf,
    mut rx: mpsc::Receiver<Command>,
    tx: mpsc::UnboundedSender<Update>,
) {
    match api.playlists().await {
        Ok(p) => emit(&tx, Update::Playlists(p)),
        Err(e) => emit(&tx, Update::Message(e.to_string(), true)),
    }
    emit(&tx, Update::Busy(false));
    let mut poll = tokio::time::interval(Duration::from_secs(5));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cover_url: Option<String> = None;
    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else { break; };
                emit(&tx, Update::Busy(true));
                if let Err(e) = execute(command, &mut api, &mut config, &dir, &tx).await { emit(&tx, Update::Message(e.to_string(), true)); }
                emit(&tx, Update::Busy(false));
            }
            _ = poll.tick() => {
                match api.playback().await {
                    Ok(p) => {
                        let url = p.item.as_ref().and_then(|t| t.album.thumbnail().map(str::to_string));
                        if url != cover_url {
                            cover_url = url.clone();
                            let cover = match &url {
                                Some(url) => api.cover_art(url).await.ok(),
                                None => None,
                            };
                            emit(&tx, Update::Cover(cover));
                        }
                        emit(&tx, Update::Playback(Box::new(p)));
                    }
                    Err(e) => emit(&tx, Update::Message(e.to_string(), true)),
                }
            }
        }
    }
}
pub async fn run(api: Api, config: Config, dir: PathBuf) -> Result<()> {
    use std::io::IsTerminal;
    anyhow::ensure!(
        std::io::stdout().is_terminal(),
        "The TUI needs an interactive terminal; use spot --help for CLI commands"
    );
    let (commands, rx) = mpsc::channel(1);
    let (updates, mut updates_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(worker(api, config.clone(), dir, rx, updates));
    let mut terminal = ratatui::init();
    let mut ui = Ui::new(config);
    let result: Result<()> = async {
        let mut events = EventStream::new(); let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            terminal.draw(|f| draw(f, &mut ui))?;
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => { if ui.key(key, &commands) { break; } }
                    Some(Err(e)) => return Err(e.into()), None => break, _ => {}
                },
                update = updates_rx.recv() => { if let Some(update) = update { ui.update(update); } else { break; } },
                _ = tick.tick() => { ui.tick(Instant::now()); }
            }
        }
        Ok(())
    }.await;
    ratatui::restore();
    task.abort();
    result
}
/// Render a deterministic offline preview using the same widgets as the live UI.
pub fn preview() -> Result<ratatui::buffer::Buffer> {
    use crate::api::{Album, Artist};
    let mut ui = Ui::new(Config::default());
    ui.busy = false;
    ui.focus = Panel::Tracks;
    ui.config.newmusic = true;
    ui.title = "New Music, week of Oct 16".into();
    ui.playlists = [
        "New Music, week of Oct 16",
        "Late nights",
        "On repeat",
        "Sunday morning",
    ]
    .iter()
    .enumerate()
    .map(|(i, name)| Playlist {
        id: i.to_string(),
        name: (*name).into(),
        description: String::new(),
    })
    .collect();
    ui.tracks = [
        ("Glasshouse", "North Shore", "After Hours", 234000),
        ("Slow Motion", "Violet Harbour", "Low Tide", 198000),
        ("Daybreak", "The Satellites", "First Light", 262000),
        ("Coastline", "Mira", "Blue Hour", 205000),
    ]
    .into_iter()
    .map(|(name, artist, album, duration_ms)| Track {
        name: name.into(),
        artists: vec![Artist {
            name: artist.into(),
            ..Default::default()
        }],
        album: Album {
            name: album.into(),
            ..Default::default()
        },
        duration_ms,
        ..Default::default()
    })
    .collect();
    ui.track_state.select(Some(1));
    ui.config.pins = vec![
        Pin {
            kind: PinKind::Track,
            uri: "spotify:track:demo".into(),
            name: ui.tracks[1].label(),
        },
        Pin {
            kind: PinKind::Playlist,
            uri: "spotify:playlist:demo".into(),
            name: "Late nights".into(),
        },
    ];
    ui.playback = Playback {
        item: Some(ui.tracks[1].clone()),
        progress_ms: Some(73000),
        is_playing: true,
        device: Some(Device {
            name: "Living room".into(),
            ..Default::default()
        }),
    };
    ui.notice("Added to New Music, week of Oct 16".into(), false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 23))?;
    terminal.draw(|frame| draw(frame, &mut ui))?;
    Ok(terminal.backend().buffer().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_mode_wakes_without_running_the_key_action() {
        let mut ui = Ui::new(Config::default());
        let (tx, mut rx) = mpsc::channel(1);
        let started = ui.last_activity;
        ui.open(Popup::Search);
        ui.input = "unfinished search".into();
        ui.tick(started + Duration::from_secs(59));
        assert!(!ui.listening);
        ui.tick(started + Duration::from_secs(60));
        assert!(ui.listening);
        assert!(!ui.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), &tx));
        assert!(!ui.listening);
        assert_eq!(ui.input, "unfinished search");
        assert!(matches!(ui.popup, Some(Popup::Search)));
        assert!(rx.try_recv().is_err());
        ui.tick(ui.last_activity + Duration::from_secs(59));
        assert!(!ui.listening);
        ui.popup = None;
        ui.key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE), &tx);
        assert!(matches!(rx.try_recv().unwrap(), Command::AddCurrent));
        ui.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE), &tx);
        assert!(ui.listening);
    }
    #[test]
    fn pinned_items_use_the_focused_selection() {
        let pin = Pin {
            kind: PinKind::Track,
            uri: "spotify:track:pinned".into(),
            name: "Pinned song".into(),
        };
        let mut ui = Ui::new(Config {
            pins: vec![pin.clone()],
            ..Default::default()
        });
        let (tx, mut rx) = mpsc::channel(1);
        ui.tracks.push(Track {
            uri: "spotify:track:other".into(),
            ..Default::default()
        });
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(matches!(rx.try_recv().unwrap(), Command::Play(uri) if uri == pin.uri));
        ui.key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT), &tx);
        assert!(matches!(rx.try_recv().unwrap(), Command::Pin(p) if p == pin));
        ui.update(Update::Pins(vec![]));
        assert_eq!(ui.pin_state.selected(), None);
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(rx.try_recv().is_err());
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        assert!(ui.focus == Panel::Playlists);
        assert!(ui.selected_track().is_none());
        ui.config.pins.push(Pin {
            kind: PinKind::Playlist,
            uri: "spotify:playlist:pinned".into(),
            name: "Playlist".into(),
        });
        ui.focus = Panel::Pins;
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert!(
            matches!(rx.try_recv().unwrap(), Command::Load(uri, _) if uri == "spotify:playlist:pinned")
        );
    }
    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy("nmc", "New Music"));
        assert!(!fuzzy("xyz", "New Music"));
    }
    #[test]
    fn empty_navigation_and_small_screen() {
        let mut ui = Ui::new(Config::default());
        move_selection(&mut ui.track_state, 0, 1);
        assert_eq!(ui.track_state.selected(), None);
        let backend = ratatui::backend::TestBackend::new(20, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
        ui.open(Popup::Help);
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
        ui.listening = true;
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
    }
}

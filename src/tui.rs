use crate::{
    api::{Api, Device, Playback, Playlist, Track},
    config::Config,
    newmusic,
};
use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph},
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
    ("j", "down", "Move down"),
    ("k", "up", "Move up"),
    ("enter", "play", "Open playlist / play track"),
    ("tab", "focus", "Switch panel"),
    ("/", "search", "Search tracks"),
    ("ctrl+p", "picker", "Find playlist or loaded track"),
    ("a", "add", "Add track to playlist"),
    ("l", "like", "Like selected track"),
    ("d", "devices", "Choose device"),
    ("+", "louder", "Volume up"),
    ("-", "quieter", "Volume down"),
    ("N", "newmusic", "Toggle new music mode"),
    ("x", "remove", "Remove selected track"),
    ("r", "recent", "Recently played"),
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
    Remove(String, String),
    Newmusic,
}
enum Update {
    Busy(bool),
    Playlists(Vec<Playlist>),
    Tracks(Vec<Track>, String, Option<String>),
    Playback(Box<Playback>),
    Devices(Vec<Device>),
    Message(String, bool),
    Newmusic(bool),
}
enum Popup {
    Help,
    Search,
    Picker,
    Add(String),
    Devices,
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
    focus: bool,
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
            focus: false,
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
                self.focus = true;
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
        }
    }
    fn open(&mut self, popup: Popup) {
        self.popup = Some(popup);
        self.input.clear();
        self.popup_state.select(Some(0));
    }
    fn selected_track(&self) -> Option<String> {
        self.tracks
            .get(self.track_state.selected().unwrap_or(0))
            .map(|t| t.uri.clone())
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
            _ => vec![],
        };
        choices
            .into_iter()
            .filter(|(name, _)| fuzzy(&self.input, name))
            .collect()
    }
    fn key(&mut self, key: KeyEvent, tx: &mpsc::Sender<Command>) -> bool {
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
                        Some(Popup::Remove(track, playlist)) if self.input == "yes" => {
                            Some(Command::Remove(track.clone(), playlist.clone()))
                        }
                        Some(Popup::Picker | Popup::Add(_) | Popup::Devices) => self
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
            "quit" => return true,
            "focus" => {
                self.focus = !self.focus;
                None
            }
            "down" | "up" => {
                let delta = if action == "down" { 1 } else { -1 };
                if self.focus {
                    move_selection(&mut self.track_state, self.tracks.len(), delta);
                } else {
                    move_selection(&mut self.playlist_state, self.playlists.len(), delta);
                }
                None
            }
            "play" if !self.focus => self
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
            "remove" => {
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
    .block(border(" Playlists ".into(), !ui.focus))
    .highlight_style(Style::default().bg(highlight))
    .highlight_symbol("› ");
    frame.render_stateful_widget(playlists, panels[0], &mut ui.playlist_state);
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
    .block(border(format!(" {} ", ui.title), ui.focus))
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
    let ratio = if duration == 0 {
        0.0
    } else {
        position as f64 / duration as f64
    };
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
        Command::Load(id, name) => {
            let tracks = api.playlist_tracks(&id).await?;
            emit(tx, Update::Tracks(tracks, name, Some(id)));
        }
        Command::Search(q) => emit(
            tx,
            Update::Tracks(
                api.search_tracks(&q).await?,
                format!("Search: {q} (up to 10)"),
                None,
            ),
        ),
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
    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else { break; };
                emit(&tx, Update::Busy(true));
                if let Err(e) = execute(command, &mut api, &mut config, &dir, &tx).await { emit(&tx, Update::Message(e.to_string(), true)); }
                emit(&tx, Update::Busy(false));
            }
            _ = poll.tick() => {
                match api.playback().await { Ok(p) => emit(&tx, Update::Playback(Box::new(p))), Err(e) => emit(&tx, Update::Message(e.to_string(), true)) }
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
                _ = tick.tick() => { ui.tick = ui.tick.wrapping_add(1); }
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
    ui.focus = true;
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
    }
}

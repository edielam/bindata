use bindata_storage::{BinaryStorage, StorageConfig};
use bytes::Bytes;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{io, path::PathBuf, time::Duration};
use tokio::sync::mpsc;

enum InputMode {
    Normal,
    Creating,
    Writing,
    Reading,
}

struct App {
    storage: BinaryStorage,
    streams: Vec<String>,
    selected_stream: Option<usize>,
    input_mode: InputMode,
    input: String,
    messages: Vec<String>,
    read_position: usize,
    read_length: usize,
}

impl App {
    async fn new() -> io::Result<App> {
        let config = StorageConfig {
            storage_dir: PathBuf::from("./storage"),
            memory_threshold: 1024 * 1024,
            max_chunk_size: 64 * 1024,
            cache_size: 100,
            segment_cleanup_threshold: Duration::from_secs(3600),
            granular_read_tracking: true,
        };

        let storage = BinaryStorage::new(config).await;
        
        Ok(App {
            storage,
            streams: Vec::new(),
            selected_stream: None,
            input_mode: InputMode::Normal,
            input: String::new(),
            messages: Vec::new(),
            read_position: 0,
            read_length: 100,
        })
    }

    async fn refresh_streams(&mut self) {
        // You'll need to implement a method to list streams in your BinaryStorage
        // This is just a placeholder
        self.streams = vec!["example_stream".to_string()];
    }
}

fn ui<B: Backend>(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(3),  // Input
            Constraint::Min(10),    // Content
            Constraint::Length(5),  // Messages
        ])
        .split(f.size());

    // Title
    let title = Paragraph::new("Binary Storage TUI")
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Input
    let input = Paragraph::new(app.input.as_ref())
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title(match app.input_mode {
            InputMode::Normal => "Normal Mode",
            InputMode::Creating => "Create Stream",
            InputMode::Writing => "Write to Stream",
            InputMode::Reading => "Read from Stream",
        }));
    f.render_widget(input, chunks[1]);

    // Content area - split into left and right
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(chunks[2]);

    // Streams list
    let streams: Vec<ListItem> = app.streams
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if Some(i) == app.selected_stream {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Spans::from(vec![Span::styled(name, style)]))
        })
        .collect();

    let streams = List::new(streams)
        .block(Block::default().borders(Borders::ALL).title("Streams"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(streams, content_chunks[0]);

    // Stream details/preview
    let details = if let Some(index) = app.selected_stream {
        format!("Selected stream: {}", app.streams[index])
    } else {
        "No stream selected".to_string()
    };

    let details = Paragraph::new(details)
        .block(Block::default().borders(Borders::ALL).title("Details"));
    f.render_widget(details, content_chunks[1]);

    // Messages
    let messages: Vec<ListItem> = app.messages
        .iter()
        .map(|m| ListItem::new(Spans::from(vec![Span::raw(m)])))
        .collect();
    let messages = List::new(messages)
        .block(Block::default().borders(Borders::ALL).title("Messages"));
    f.render_widget(messages, chunks[3]);
}

async fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    mut rx: mpsc::Receiver<Event>,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::<B>(f, &app))?;

        if let Ok(event) = rx.try_recv() {
            match event {
                Event::Key(key) => {
                    match app.input_mode {
                        InputMode::Normal => {
                            match key.code {
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Char('c') => {
                                    app.input_mode = InputMode::Creating;
                                    app.input.clear();
                                }
                                KeyCode::Char('w') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Writing;
                                        app.input.clear();
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Reading;
                                        app.input.clear();
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(selected) = app.selected_stream {
                                        if selected < app.streams.len() - 1 {
                                            app.selected_stream = Some(selected + 1);
                                        }
                                    } else if !app.streams.is_empty() {
                                        app.selected_stream = Some(0);
                                    }
                                }
                                KeyCode::Up => {
                                    if let Some(selected) = app.selected_stream {
                                        if selected > 0 {
                                            app.selected_stream = Some(selected - 1);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {
                            match key.code {
                                KeyCode::Enter => {
                                    match app.input_mode {
                                        InputMode::Creating => {
                                            let stream_id = app.input.clone();
                                            if let Err(e) = app.storage.create_stream(stream_id.clone()).await {
                                                app.messages.push(format!("Error: {}", e));
                                            } else {
                                                app.messages.push(format!("Created stream: {}", stream_id));
                                                app.refresh_streams().await;
                                            }
                                        }
                                        InputMode::Writing => {
                                            if let Some(index) = app.selected_stream {
                                                let stream_id = &app.streams[index];
                                                let data = Bytes::from(app.input.as_bytes().to_vec());
                                                match app.storage.append_data_to_stream(stream_id, data).await {
                                                    Ok(pos) => app.messages.push(format!("Wrote data at position {}", pos)),
                                                    Err(e) => app.messages.push(format!("Error: {}", e)),
                                                }
                                            }
                                        }
                                        InputMode::Reading => {
                                            if let Some(index) = app.selected_stream {
                                                let stream_id = &app.streams[index];
                                                match app.storage.read_data_from_stream(
                                                    stream_id,
                                                    app.read_position,
                                                    app.read_length,
                                                ).await {
                                                    Ok(data) => {
                                                        if let Ok(text) = String::from_utf8(data.to_vec()) {
                                                            app.messages.push(format!("Read: {}", text));
                                                        } else {
                                                            app.messages.push(format!("Read {} bytes of binary data", data.len()));
                                                        }
                                                    }
                                                    Err(e) => app.messages.push(format!("Error: {}", e)),
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                    app.input_mode = InputMode::Normal;
                                    app.input.clear();
                                }
                                KeyCode::Char(c) => {
                                    app.input.push(c);
                                }
                                KeyCode::Backspace => {
                                    app.input.pop();
                                }
                                KeyCode::Esc => {
                                    app.input_mode = InputMode::Normal;
                                    app.input.clear();
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Terminal initialization
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create a channel for handling events
    let (tx, rx) = mpsc::channel(32);
    let event_tx = tx.clone();

    // Spawn event handling thread
    tokio::spawn(async move {
        loop {
            if event::poll(std::time::Duration::from_millis(100)).unwrap() {
                if let Ok(event) = event::read() {
                    let _ = event_tx.send(event).await;
                }
            }
        }
    });

    // Create app and run it
    let app = App::new().await?;
    let res = run_app(&mut terminal, app, rx).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}
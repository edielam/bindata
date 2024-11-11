use bindata_storage::{BinaryStorage, StorageConfig};
use bytes::Bytes;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{io, path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncReadExt;
use tokio::fs::File as TokioFile;
use serde_json::Value;
use std::collections::HashMap;
use chrono::{DateTime, Local};

#[derive(Debug)]
struct StreamMetadata {
    id: String,
    created_at: String,
    total_size: u64,
    last_modified: Option<DateTime<Local>>,
}

enum InputMode {
    Normal,
    Creating,
    Writing,
    Reading,
    Help,
}

struct App {
    storage: BinaryStorage,
    streams: Vec<String>,
    selected_stream: Option<usize>,
    input_mode: InputMode,
    input: String,
    messages: Vec<(String, Color)>, 
    read_position: usize,
    read_length: usize,
    upload_progress: Option<(String, u64, u64)>,
    is_loading: bool,
    stream_metadata: HashMap<String, StreamMetadata>,
    help_visible: bool,
    stream_read_positions: HashMap<String, u64>,
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
        
        let mut app = App {
            storage,
            streams: Vec::new(),
            selected_stream: None,
            input_mode: InputMode::Normal,
            input: String::new(),
            messages: Vec::new(),
            read_position: 0,
            read_length: 1024 * 64,
            upload_progress: None,
            is_loading: false,
            stream_metadata: HashMap::new(),
            help_visible: false,
            stream_read_positions: HashMap::new(),
        };
        
        app.add_message("Welcome to Binary Storage CLI! Press 'h' for help.".to_string(), Color::Cyan);
        app.refresh_streams().await;
        
        Ok(app)
    }

    fn add_message(&mut self, message: String, color: Color) {
        self.messages.push((message, color));
        if self.messages.len() > 100 {
            self.messages.remove(0);
        }
    }

    async fn refresh_streams(&mut self) {
        self.is_loading = true;
        let mut streams = Vec::new();
        let mut metadata = HashMap::new();
        
        if let Ok(mut entries) = tokio::fs::read_dir(&self.storage.config.storage_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(file_type) = entry.file_type().await {
                    if file_type.is_dir() {
                        if let Some(dir_name) = entry.file_name().to_str() {
                            let meta_path = entry.path().join("stream.meta");
                            if let Ok(meta_content) = tokio::fs::read_to_string(&meta_path).await {
                                if let Ok(meta_json) = serde_json::from_str::<Value>(&meta_content) {
                                    if let Some(id) = meta_json["id"].as_str() {
                                        let last_modified = tokio::fs::metadata(&meta_path)
                                            .await
                                            .ok()
                                            .and_then(|m| m.modified().ok())
                                            .map(|t| DateTime::from(t));
                                        
                                        streams.push(id.to_string());
                                        metadata.insert(id.to_string(), StreamMetadata {
                                            id: id.to_string(),
                                            created_at: meta_json["created_at"].as_str()
                                                .unwrap_or("unknown").to_string(),
                                            total_size: meta_json["total_size"].as_u64().unwrap_or(0),
                                            last_modified: last_modified,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        streams.sort();
        self.streams = streams;
        self.stream_metadata = metadata;
        
        if let Some(selected) = self.selected_stream {
            if selected >= self.streams.len() {
                self.selected_stream = None;
            }
        }
        
        self.is_loading = false;
    }

    async fn create_stream(&mut self, stream_id: String) -> io::Result<()> {
        if stream_id.trim().is_empty() {
            self.add_message("Stream ID cannot be empty".to_string(), Color::Red);
            return Ok(());
        }
        
        if self.streams.contains(&stream_id) {
            self.add_message(format!("Stream '{}' already exists", stream_id), Color::Red);
            return Ok(());
        }
        
        let _ = self.storage.create_stream(stream_id.clone()).await;
        self.add_message(format!("Created stream: {}", stream_id), Color::Green);
        self.refresh_streams().await;
        Ok(())
    }

    async fn upload_file(&mut self, path: &str) -> io::Result<()> {
        if let Some(stream_index) = self.selected_stream {
            let stream_id = &self.streams[stream_index];
            
            // Validate file exists
            if !std::path::Path::new(path).exists() {
                self.add_message(format!("File not found: {}", path), Color::Red);
                return Ok(());
            }
            
            let mut file = TokioFile::open(path).await?;
            let metadata = file.metadata().await?;
            let total_size = metadata.len();
            
            let pb = ProgressBar::new(total_size);
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("█▓▒░"));
            
            self.upload_progress = Some((path.to_string(), 0, total_size));
            
            let mut buffer = vec![0u8; 64 * 1024];
            let mut current_size = 0;
            
            loop {
                let n = file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                
                let chunk = Bytes::from(buffer[..n].to_vec());
                let _ = self.storage.append_data_to_stream(stream_id, chunk).await;
                
                current_size += n as u64;
                pb.set_position(current_size);
                self.upload_progress = Some((path.to_string(), current_size, total_size));
            }
            
            pb.finish_with_message("Upload complete");
            self.upload_progress = None;
            self.add_message(
                format!("Successfully uploaded {} ({} bytes) to stream {}", 
                    path, 
                    total_size,
                    stream_id
                ),
                Color::Green
            );
            self.refresh_streams().await;
        }
        Ok(())
    }
    async fn read_stream_data(&mut self, stream_id: &str) -> io::Result<()> {
        // Get the stream's total size
        let total_size = self.stream_metadata
            .get(stream_id)
            .map(|meta| meta.total_size)
            .unwrap_or(0);

        if total_size == 0 {
            self.add_message(format!("Stream '{}' is empty", stream_id), Color::Yellow);
            return Ok(());
        }

        // Reset or initialize read position for this stream
        let start_position = 0;
        self.stream_read_positions.insert(stream_id.to_string(), start_position);
        
        let mut current_position = start_position;
        let chunk_size = self.read_length;

        self.add_message(
            format!("Reading stream '{}' (total size: {} bytes)", stream_id, total_size),
            Color::Cyan
        );

        while current_position < total_size {
            let remaining = total_size - current_position;
            let read_size = std::cmp::min(chunk_size, remaining as usize);

            match self.storage.read_data_from_stream(
                stream_id,
                current_position as usize,
                read_size,
            ).await {
                Ok(data) => {
                    if data.is_empty() {
                        break;
                    }

                    // Try to display as text, fall back to binary length
                    match String::from_utf8(data.to_vec()) {
                        Ok(text) => {
                            self.add_message(
                                format!("[Pos {}] {}", current_position, text),
                                Color::White
                            );
                        }
                        Err(_) => {
                            self.add_message(
                                format!("[Pos {}] Binary data: {} bytes", current_position, data.len()),
                                Color::White
                            );
                        }
                    }

                    current_position += data.len() as u64;
                    self.stream_read_positions.insert(stream_id.to_string(), current_position);
                }
                Err(e) => {
                    self.add_message(
                        format!("Error reading at position {}: {}", current_position, e),
                        Color::Red
                    );
                    break;
                }
            }
        }

        self.add_message(
            format!("Finished reading stream '{}' ({}/{} bytes)", 
                stream_id, 
                current_position, 
                total_size
            ),
            Color::Green
        );
        Ok(())
    }
}

fn ui<B: Backend>(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(3),  // Input
            Constraint::Min(10),    // Content
            Constraint::Length(8),  // Messages
        ])
        .split(f.size());

    // Title with styled components
    let title = Line::from(vec![
        Span::styled("Binary ", Style::default().fg(Color::Cyan)),
        Span::styled("Data ", Style::default().fg(Color::White)),
        Span::styled("Storage ", Style::default().fg(Color::Cyan)),
        Span::styled("CLI", Style::default().fg(Color::White)),
    ]);
    
    let title = Paragraph::new(title)
        .alignment(Alignment::Center)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)));
    f.render_widget(title, chunks[0]);

    // Input with mode indicator
    let mode_style = match app.input_mode {
        InputMode::Normal => Style::default().fg(Color::Green),
        InputMode::Creating => Style::default().fg(Color::Yellow),
        InputMode::Writing => Style::default().fg(Color::Blue),
        InputMode::Reading => Style::default().fg(Color::Magenta),
        InputMode::Help => Style::default().fg(Color::Cyan),
    };
    
    let mode_text = match app.input_mode {
        InputMode::Normal => "NORMAL",
        InputMode::Creating => "CREATE",
        InputMode::Writing => "WRITE",
        InputMode::Reading => "READ",
        InputMode::Help => "HELP",
    };

    let input = Paragraph::new(Line::from(app.input.as_ref()))
        .style(Style::default())
        .block(Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(mode_text, mode_style)));
    f.render_widget(input, chunks[1]);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(chunks[2]);

    // Streams list with metadata
    let streams: Vec<ListItem> = app.streams
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let meta = app.stream_metadata.get(name);
            let size_text = meta.map_or("".to_string(), |m| 
                format!(" ({} bytes)", m.total_size)
            );
            
            let style = if Some(i) == app.selected_stream {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            
            let mut spans = vec![
                Span::styled(name, style),
                Span::styled(&size_text, Style::default().fg(Color::DarkGray))
            ];
            
            if let Some(meta) = meta {
                if let Some(modified) = meta.last_modified {
                    spans.push(Span::raw("\n"));
                    spans.push(Span::styled(
                        format!("Modified: {}", modified.format("%Y-%m-%d %H:%M")),
                        Style::default().fg(Color::DarkGray)
                    ));
                }
            }
            
            ListItem::new(Line::from(vec![Span::styled(name, style)]))
        })
        .collect();

    let streams_title = if app.is_loading {
        "Streams (Loading...)"
    } else {
        "Streams"
    };

    let streams = List::new(streams)
        .block(Block::default()
            .borders(Borders::ALL)
            .title(streams_title))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        );
    f.render_widget(streams, content_chunks[0]);

    // Details or Help panel
    if app.help_visible {
        let help_text = vec![
            "Keyboard Controls:",
            "q - Quit",
            "h - Toggle Help",
            "c - Create Stream",
            "w - Write to Stream",
            "r - Read from Stream",
            "u - Upload File",
            "f - Refresh Streams",
            "↑/↓ - Navigate Streams",
            "ESC - Cancel Input",
        ];
        
        let help = Paragraph::new(Text::from(help_text.join("\n")))
            .block(Block::default()
                .borders(Borders::ALL)
                .title("Help"))
            .wrap(Wrap { trim: true });
        f.render_widget(help, content_chunks[1]);
    } else {
        let details = if let Some((filename, current, total)) = &app.upload_progress {
            let percentage = (current * 100) / total;
            let progress_bar = format!(
                "Uploading: {}\n[{}{}] {}%",
                filename,
                "█".repeat((percentage as usize / 2).min(50)),
                "░".repeat(50 - (percentage as usize / 2).min(50)),
                percentage
            );
            Paragraph::new(Text::from(progress_bar))
        } else if let Some(index) = app.selected_stream {
            let stream_id = &app.streams[index];
            let details_text = if let Some(metadata) = app.stream_metadata.get(stream_id) {
                format!(
                    "Stream ID: {}\nCreated: {}\nTotal Size: {} bytes{}",
                    metadata.id,
                    metadata.created_at,
                    metadata.total_size,
                    metadata.last_modified.map_or(String::new(), |dt| 
                        format!("\nLast Modified: {}", dt.format("%Y-%m-%d %H:%M:%S"))
                    )
                )
            } else {
                format!("Selected stream: {}\nNo metadata available", stream_id)
            };
            Paragraph::new(Text::from(details_text))
        } else {
            Paragraph::new(Text::from("No stream selected"))
        };

        let details = details
            .block(Block::default()
                .borders(Borders::ALL)
                .title("Details"))
            .wrap(Wrap { trim: true });
        f.render_widget(details, content_chunks[1]);
    }

    let messages_len = app.messages.len();
    let messages_height = chunks[3].height as usize - 2; 
    let start_index = if messages_len > messages_height {
        messages_len - messages_height
    } else {
        0
    };
    let messages: Vec<ListItem> = app.messages
        .iter()
        .skip(start_index) // Skip older messages that won't fit
        .map(|(msg, color)| {
            ListItem::new(Line::from(vec![
                Span::styled(msg, Style::default().fg(*color))
            ]))
        })
        .collect();
    
    let messages = List::new(messages.clone())
        .block(Block::default()
            .borders(Borders::ALL)
            .title(format!("Messages ({}/{})", messages_len, start_index + messages.len())))
        .start_corner(ratatui::layout::Corner::TopLeft); // Changed from BottomLeft
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
                                KeyCode::Char('h') => {
                                    app.help_visible = !app.help_visible;
                                }
                                KeyCode::Char('c') => {
                                    app.input_mode = InputMode::Creating;
                                    app.input.clear();
                                    app.add_message("Enter stream ID to create".to_string(), Color::Yellow);
                                }
                                KeyCode::Char('w') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Writing;
                                        app.input.clear();
                                        app.add_message(
                                            "Enter text to write or 'file:path' to upload a file".to_string(),
                                            Color::Blue
                                        );
                                    } else {
                                        app.add_message("Please select a stream first".to_string(), Color::Red);
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Reading;
                                        app.input.clear();
                                        app.add_message(
                                            "Press Enter to read next chunk".to_string(),
                                            Color::Magenta
                                        );
                                    } else {
                                        app.add_message("Please select a stream first".to_string(), Color::Red);
                                    }
                                }
                                KeyCode::Char('u') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Writing;
                                        app.input.clear();
                                        app.add_message(
                                            "Enter file path to upload (prefix with 'file:')".to_string(),
                                            Color::Blue
                                        );
                                    } else {
                                        app.add_message("Please select a stream first".to_string(), Color::Red);
                                    }
                                }
                                KeyCode::Char('f') => {
                                    app.add_message("Refreshing streams...".to_string(), Color::Cyan);
                                    app.refresh_streams().await;
                                    app.add_message("Streams refreshed".to_string(), Color::Green);
                                }
                                KeyCode::Down => {
                                    if let Some(selected) = app.selected_stream {
                                        if selected < app.streams.len().saturating_sub(1) {
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
                                    } else if !app.streams.is_empty() {
                                        app.selected_stream = Some(app.streams.len() - 1);
                                    }
                                }
                                KeyCode::PageDown => {
                                    if let Some(selected) = app.selected_stream {
                                        let new_selected = (selected + 5).min(app.streams.len().saturating_sub(1));
                                        app.selected_stream = Some(new_selected);
                                    }
                                }
                                KeyCode::PageUp => {
                                    if let Some(selected) = app.selected_stream {
                                        let new_selected = selected.saturating_sub(5);
                                        app.selected_stream = Some(new_selected);
                                    }
                                }
                                _ => {}
                            }
                        }
                        InputMode::Creating => {
                            match key.code {
                                KeyCode::Enter => {
                                    if !app.input.is_empty() {
                                        if let Err(e) = app.create_stream(app.input.clone()).await {
                                            app.add_message(format!("Error creating stream: {}", e), Color::Red);
                                        }
                                        app.input_mode = InputMode::Normal;
                                        app.input.clear();
                                    }
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
                                    app.add_message("Cancelled stream creation".to_string(), Color::Yellow);
                                }
                                _ => {}
                            }
                        }
                        InputMode::Writing | InputMode::Reading => {
                            match key.code {
                                KeyCode::Enter => {
                                    match app.input_mode {
                                        InputMode::Writing => {
                                            let input = app.input.clone();
                                            if input.starts_with("file:") {
                                                let path = input.trim_start_matches("file:").trim();
                                                if let Err(e) = app.upload_file(path).await {
                                                    app.add_message(format!("Upload error: {}", e), Color::Red);
                                                }
                                            } else {
                                                if let Some(index) = app.selected_stream {
                                                    let stream_id = &app.streams[index];
                                                    let data = Bytes::from(app.input.as_bytes().to_vec());
                                                    match app.storage.append_data_to_stream(stream_id, data.clone()).await {
                                                        Ok(pos) => app.add_message(
                                                            format!("Wrote {} bytes at position {}", data.len(), pos),
                                                            Color::Green
                                                        ),
                                                        Err(e) => app.add_message(format!("Error: {}", e), Color::Red),
                                                    }
                                                }
                                            }
                                        }
                                        InputMode::Reading => {
                                            match key.code {
                                                KeyCode::Enter => {
                                                    if let Some(index) = app.selected_stream {
                                                        let stream_id = app.streams[index].clone();
                                                        if let Err(e) = app.read_stream_data(&stream_id).await {
                                                            app.add_message(
                                                                format!("Error reading stream: {}", e),
                                                                Color::Red
                                                            );
                                                        }
                                                        app.input_mode = InputMode::Normal;
                                                        app.input.clear();
                                                    }
                                                }
                                                KeyCode::Esc => {
                                                    app.input_mode = InputMode::Normal;
                                                    app.input.clear();
                                                    app.add_message("Reading cancelled".to_string(), Color::Yellow);
                                                }
                                                _ => {}
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
                                    app.add_message("Operation cancelled".to_string(), Color::Yellow);
                                }
                                _ => {}
                            }
                        }
                        InputMode::Help => {
                            if let KeyCode::Esc = key.code {
                                app.input_mode = InputMode::Normal;
                                app.help_visible = false;
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
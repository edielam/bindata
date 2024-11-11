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
    text::{Span, Line},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{io, path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncReadExt;
use tokio::fs::File as TokioFile;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug)]
struct StreamMetadata {
    id: String,
    created_at: String,
    total_size: u64,
}
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
    upload_progress: Option<(String, u64, u64)>, // (filename, current, total)
    is_loading: bool,
    stream_metadata: HashMap<String, StreamMetadata>,
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
            read_length: 100,
            upload_progress: None,
            is_loading: false,
            stream_metadata: HashMap::new(),
        };
        
        app.refresh_streams().await;
        
        Ok(app)
        
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
                                        streams.push(id.to_string());
                                        metadata.insert(id.to_string(), StreamMetadata {
                                            id: id.to_string(),
                                            created_at: meta_json["created_at"].as_str()
                                                .unwrap_or("unknown").to_string(),
                                            total_size: meta_json["total_size"].as_u64().unwrap_or(0),
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
        let _ = self.storage.create_stream(stream_id.clone()).await;
        self.messages.push(format!("Created stream: {}", stream_id));
        self.refresh_streams().await;
        Ok(())
    }


    async fn upload_file(&mut self, path: &str) -> io::Result<()> {
        if let Some(stream_index) = self.selected_stream {
            let stream_id = &self.streams[stream_index];
            
            // Open file and get its size
            let mut file = TokioFile::open(path).await?;
            let metadata = file.metadata().await?;
            let total_size = metadata.len();
            
            // Create a progress bar
            let pb = ProgressBar::new(total_size);
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"));
            
            // Update the UI to show upload progress
            self.upload_progress = Some((path.to_string(), 0, total_size));
            
            // Use a buffer for reading chunks
            let mut buffer = vec![0u8; 64 * 1024]; // 64KB chunks
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
            self.messages.push(format!("Successfully uploaded {} to stream {}", path, stream_id));
        }
        Ok(())
    }
    // async fn handle_upload(&mut self, path: &str) -> io::Result<()> {
    //     // Verify file exists
    //     if !tokio::fs::metadata(path).await.is_ok() {
    //         self.messages.push(format!("Error: File '{}' not found", path));
    //         return Ok(());
    //     }
        
    //     // Start upload
    //     match self.upload_file(path).await {
    //         Ok(_) => {
    //             self.messages.push(format!("Upload complete: {}", path));
    //             self.refresh_streams().await;
    //         }
    //         Err(e) => {
    //             self.messages.push(format!("Upload error: {}", e));
    //         }
    //     }
    //     Ok(())
    // }
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
    let title = Paragraph::new(Line::from("Binary Data Storage"))
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Input
    let input = Paragraph::new(Line::from(app.input.as_ref()))
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
            ListItem::new(Line::from(vec![Span::styled(name, style)]))
        })
        .collect();
    let streams_title = if app.is_loading {
            "Streams (Loading...)"
        } else {
            "Streams"
        };

    let streams = List::new(streams)
        .block(Block::default().borders(Borders::ALL).title(streams_title))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(streams, content_chunks[0]);

    let details = if let Some((filename, current, total)) = &app.upload_progress {
        let percentage = (current * 100) / total;
        let progress_bar = format!(
            "Uploading: {}\n[{}] {}%",
            filename,
            "=".repeat((percentage as usize / 2).min(50)),
            percentage
        );
        Paragraph::new(Line::from(progress_bar))
    } else if let Some(index) = app.selected_stream {
        let stream_id = &app.streams[index];
        let details_text = if let Some(metadata) = app.stream_metadata.get(stream_id) {
            format!(
                "Stream ID: {}\nCreated: {}\nTotal Size: {} bytes",
                metadata.id,
                metadata.created_at,
                metadata.total_size
            )
        } else {
            format!("Selected stream: {}\nNo metadata available", stream_id)
        };
        Paragraph::new(Line::from(details_text))
    } else {
        Paragraph::new(Line::from("No stream selected"))
    };

    let details = details.block(Block::default().borders(Borders::ALL).title("Details"));
    f.render_widget(details, content_chunks[1]);

    // Messages
    let messages: Vec<ListItem> = app.messages
        .iter()
        .map(|m| ListItem::new(Line::from(vec![Span::raw(m)])))
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
                                KeyCode::Char('u') => {
                                    if app.selected_stream.is_some() {
                                        app.input_mode = InputMode::Writing;
                                        app.input.clear();
                                        app.messages.push("Enter path to file to upload (prefix with 'file:'):".to_string());
                                    } else {
                                        app.messages.push("Please select a stream first".to_string());
                                    }
                                }
                                KeyCode::Char('f') => {
                                    app.messages.push("Refreshing streams...".to_string());
                                    app.refresh_streams().await;
                                    app.messages.push("Streams refreshed".to_string());
                                }
                                _ => {}
                            }
                        }
                        InputMode::Creating => {
                            match key.code {
                                KeyCode::Enter => {
                                    if !app.input.is_empty() {
                                        if let Err(e) = app.create_stream(app.input.clone()).await {
                                            app.messages.push(format!("Error creating stream: {}", e));
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
                                            let input = app.input.clone();
                                            if input.starts_with("file:") {
                                                let path = input.trim_start_matches("file:").trim();
                                                if let Err(e) = app.upload_file(path).await {
                                                    app.messages.push(format!("Upload error: {}", e));
                                                }
                                            } else {
                                                // Handle normal text input as before
                                                if let Some(index) = app.selected_stream {
                                                    let stream_id = &app.streams[index];
                                                    let data = Bytes::from(app.input.as_bytes().to_vec());
                                                    match app.storage.append_data_to_stream(stream_id, data).await {
                                                        Ok(pos) => app.messages.push(format!("Wrote data at position {}", pos)),
                                                        Err(e) => app.messages.push(format!("Error: {}", e)),
                                                    }
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
use bindata_storage::{BinaryStorage, StorageConfig};
use bytes::Bytes;
use std::path::PathBuf;
use std::time::Duration;
use tokio;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {

    #[arg(short, long, default_value = "./storage")]
    storage_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Create {
        stream_id: String,
    },
    Write {
        stream_id: String,
        file: PathBuf,
    },
    Read {
        stream_id: String,
        position: usize,
        length: usize,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.storage_dir)?;

    let config = StorageConfig {
        storage_dir: cli.storage_dir,
        memory_threshold: 1024 * 1024, // 1MB
        max_chunk_size: 64 * 1024,     // 64KB
        cache_size: 100,
        segment_cleanup_threshold: Duration::from_secs(3600), // 1 hour
        granular_read_tracking: true,
    };

    let storage = BinaryStorage::new(config).await;

    match cli.command {
        Commands::Create { stream_id } => {
            storage.create_stream(stream_id.clone()).await?;
            println!("Created stream: {}", stream_id);
        }

        Commands::Write { stream_id, file } => {
            let data = tokio::fs::read(&file).await?;
            let bytes = Bytes::from(data);
            let position = storage.append_data_to_stream(&stream_id, bytes.clone()).await?;
            println!("Wrote {} bytes to stream {} at position {}", bytes.len(), stream_id, position);
        }

        Commands::Read { stream_id, position, length, output } => {
            let data = storage.read_data_from_stream(&stream_id, position, length).await?;
            
            match output {
                Some(path) => {
                    tokio::fs::write(path, data).await?;
                    println!("Read {} bytes from position {} and wrote to file", length, position);
                }
                None => {
                    if let Ok(text) = String::from_utf8(data.to_vec()) {
                        println!("{}", text);
                    } else {
                        // Print as hex if not valid UTF-8
                        for byte in data.iter() {
                            print!("{:02x}", byte);
                        }
                        println!();
                    }
                }
            }
        }
    }

    Ok(())
}
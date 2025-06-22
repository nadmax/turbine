use clap::{Parser, Subcommand};
use std::path::PathBuf;
use turbine::{ContainerConfig, TurbineRuntime, Result};

#[derive(Parser)]
#[command(name = "turbine")]
#[command(about = "A lightweight container runtime for web applications")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[arg(long, default_value = "/tmp/turbine")]
    base_path: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    Create {
        #[arg(short, long)]
        config: String,
        
        #[arg(short, long)]
        name: Option<String>,
    },
    Start {
        container_id: String,
    },
    Stop {
        container_id: String,
        
        #[arg(short, long)]
        force: bool,
    },
    Restart {
        container_id: String,
    },
    Remove {
        container_id: String,
        
        #[arg(short, long)]
        force: bool,
    },
    List,
    Logs {
        container_id: String,
    },
    Exec {
        container_id: String,
        command: Vec<String>,
    },
    Stats {
        container_id: String,
    },
    Pause {
        container_id: String,
    },
    Resume {
        container_id: String,
    },
    Deploy {
        #[arg(short, long)]
        name: String,
        
        #[arg(short, long)]
        image: String,
        
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
    Cleanup,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = TurbineRuntime::new(&cli.base_path);

    runtime.initialize().await?;

    match cli.command {
        Commands::Create { config, name } => {
            let mut container_config = ContainerConfig::from_file(&config)?;
            if let Some(name) = name {
                container_config.name = name;
            }

            let container_id = runtime.create_container(container_config).await?;
            println!("Container created: {}", container_id);
        }

        Commands::Start { container_id } => {
            runtime.start_container(&container_id).await?;
            println!("Container started: {}", container_id);
        }

        Commands::Stop { container_id, force } => {
            runtime.stop_container(&container_id, force).await?;
            println!("Container stopped: {}", container_id);
        }

        Commands::Restart { container_id } => {
            runtime.restart_container(&container_id).await?;
            println!("Container restarted: {}", container_id);
        }

        Commands::Remove { container_id, force } => {
            runtime.remove_container(&container_id, force).await?;
            println!("Container removed: {}", container_id);
        }

        Commands::List => {
            let containers = runtime.list_containers().await?;
            if containers.is_empty() {
                println!("No containers found");
            } else {
                println!("{:<12} {:<20} {:<15} {:<10}", "ID", "NAME", "IMAGE", "STATUS");
                println!("{}", "-".repeat(60));

                for container in containers {
                    let short_id = &container.id[..8];
                    let status = format!("{:?}", container.state);

                    println!("{:<12} {:<20} {:<15} {:<10}", 
                        short_id, 
                        container.config.name, 
                        container.config.image,
                        status
                    );
                }
            }
        }

        Commands::Logs { container_id } => {
            let (stdout, stderr) = runtime.get_container_logs(&container_id).await?;            
            if !stdout.is_empty() {
                println!("STDOUT:");
                println!("{}", stdout);
            }

            if !stderr.is_empty() {
                println!("STDERR:");
                println!("{}", stderr);
            }
        }

        Commands::Exec { container_id, command } => {
            let output = runtime.execute_in_container(&container_id, command).await?;

            println!("{}", output);
        }

        Commands::Stats { container_id } => {
            let stats = runtime.get_container_stats(&container_id).await?;

            println!("Container: {}", stats.container_id);
            println!("Memory Usage: {} MB", stats.memory_usage / 1024 / 1024);
            println!("CPU Usage: {:.2}%", stats.cpu_usage);
            println!("Network RX: {} bytes", stats.network_rx);
            println!("Network TX: {} bytes", stats.network_tx);
            println!("Uptime: {} seconds", stats.uptime);
        }

        Commands::Pause { container_id } => {
            runtime.pause_container(&container_id).await?;
            println!("Container paused: {}", container_id);
        }

        Commands::Resume { container_id } => {
            runtime.resume_container(&container_id).await?;
            println!("Container resumed: {}", container_id);
        }

        Commands::Deploy { name, image, port } => {
            let container_id = runtime.deploy_web_app(name.clone(), image, port).await?;

            println!("Web application '{}' deployed: {}", name, container_id);
            println!("Access your application at: http://localhost:{}", port);
        }

        Commands::Cleanup => {
            runtime.cleanup().await?;
            println!("Cleanup completed");
        }
    }

    Ok(())
}

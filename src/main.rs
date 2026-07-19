mod attach;
mod auth;
mod config;
mod host;
mod http_utils;
mod protocol;
mod state;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "locho", about = "Private HTTP and TCP service tunnel")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Host {
        #[arg(long)]
        config: PathBuf,
    },
    ResetIdentity,
    RotateSecret {
        service: String,
    },
    Attach {
        host_id: String,
        service: String,
        secret: String,
        #[arg(long)]
        tcp: bool,
        #[arg(long, default_value = "127.0.0.1:8765")]
        listen: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    match Cli::parse().command {
        Command::Host { config } => host::run(config).await,
        Command::ResetIdentity => state::reset_identity(),
        Command::RotateSecret { service } => state::rotate_secret(&service),
        Command::Attach {
            host_id,
            service,
            secret,
            tcp,
            listen,
        } => attach::run(host_id, service, secret, tcp, listen).await,
    }
}

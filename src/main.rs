mod attach;
mod auth;
mod host;
mod http_utils;
mod protocol;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use url::Url;

#[derive(Parser)]
#[command(name = "locho", about = "HTTP reverse proxy over an iroh tunnel")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Host {
        #[arg(long, default_value = "https://example.com")]
        upstream: Url,
        #[arg(long, help = "Reusable attach secret; generated randomly when omitted")]
        secret: Option<String>,
    },
    Attach {
        host_id: String,
        secret: String,
        #[arg(long, default_value = "127.0.0.1:8765")]
        listen: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    match Cli::parse().command {
        Command::Host { upstream, secret } => host::run(upstream, secret).await,
        Command::Attach {
            host_id,
            secret,
            listen,
        } => attach::run(host_id, secret, listen).await,
    }
}

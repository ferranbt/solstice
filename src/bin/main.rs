use clap::Parser;
use solstice::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_ansi(false) // Disable ANSI colors
        .init();

    cli.run().await;
}

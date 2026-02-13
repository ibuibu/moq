use anyhow::Result;
use wtransport::ClientConfig;
use wtransport::Endpoint;

use moq::publisher::Publisher;
use moq::session::client_setup;
use moq::subscriber::Subscriber;

const SERVER_URL: &str = "https://localhost:4433";
const TRACK_NAMESPACE: &str = "live";
const TRACK_NAME: &str = "chat";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match mode {
        "publish" => run_publisher().await,
        "subscribe" => run_subscriber().await,
        _ => {
            println!("Usage: moq-client <publish|subscribe>");
            Ok(())
        }
    }
}

async fn connect() -> Result<wtransport::Connection> {
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();

    let endpoint = Endpoint::client(config)?;
    let connection = endpoint.connect(SERVER_URL).await?;
    tracing::info!("connected to {SERVER_URL}");
    Ok(connection)
}

async fn run_publisher() -> Result<()> {
    let connection = connect().await?;
    let (control_send, control_recv) = client_setup(&connection).await?;

    let mut publisher = Publisher::new(
        connection,
        control_send,
        control_recv,
        TRACK_NAMESPACE.to_string(),
        TRACK_NAME.to_string(),
    )
    .await?;

    tracing::info!("publisher ready. type messages and press Enter:");

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);

    use tokio::io::AsyncBufReadExt;
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }
        publisher.send_object(line.as_bytes()).await?;
        tracing::info!("sent: {line}");
    }

    Ok(())
}

async fn run_subscriber() -> Result<()> {
    let connection = connect().await?;
    let (control_send, control_recv) = client_setup(&connection).await?;

    let subscriber = Subscriber::new(
        connection,
        control_send,
        control_recv,
        TRACK_NAMESPACE.to_string(),
        TRACK_NAME.to_string(),
    )
    .await?;

    tracing::info!("subscriber ready. waiting for objects...");

    loop {
        let (header, payload) = subscriber.recv_object().await?;
        let text = String::from_utf8_lossy(&payload);
        println!(
            "[group={} id={}] {}",
            header.group_id, header.object_id, text
        );
    }
}

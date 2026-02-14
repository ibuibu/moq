use std::io::Cursor;
use std::time::Duration;

use anyhow::Result;
use wtransport::ClientConfig;
use wtransport::Endpoint;

use moq::publisher::Publisher;
use moq::session::client_setup;
use moq::subscriber::Subscriber;

const SERVER_URL: &str = "https://localhost:4433";
const TRACK_NAMESPACE: &str = "live";
const TRACK_NAME: &str = "chat";
const VIDEO_TRACK_NAME: &str = "video";
const VIDEO_WIDTH: u32 = 640;
const VIDEO_HEIGHT: u32 = 480;
const VIDEO_FPS: u64 = 15;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match mode {
        "publish" => run_publisher().await,
        "subscribe" => run_subscriber().await,
        "video-publish" => run_video_publisher().await,
        "video-subscribe" => run_video_subscriber().await,
        _ => {
            println!("Usage: moq-client <publish|subscribe|video-publish|video-subscribe>");
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

async fn run_video_publisher() -> Result<()> {
    use image::codecs::jpeg::JpegEncoder;
    use image::RgbImage;

    let connection = connect().await?;
    let (control_send, control_recv) = client_setup(&connection).await?;

    let mut publisher = Publisher::new(
        connection,
        control_send,
        control_recv,
        TRACK_NAMESPACE.to_string(),
        VIDEO_TRACK_NAME.to_string(),
    )
    .await?;

    tracing::info!("starting test pattern video publisher ({}x{} @ {}fps)", VIDEO_WIDTH, VIDEO_HEIGHT, VIDEO_FPS);

    let mut interval = tokio::time::interval(Duration::from_millis(1000 / VIDEO_FPS));
    let mut frame_count: u64 = 0;

    // カラーバーの色 (SMPTE風)
    let colors: [(u8, u8, u8); 7] = [
        (192, 192, 192), // 白
        (192, 192, 0),   // 黄
        (0, 192, 192),   // シアン
        (0, 192, 0),     // 緑
        (192, 0, 192),   // マゼンタ
        (192, 0, 0),     // 赤
        (0, 0, 192),     // 青
    ];

    loop {
        interval.tick().await;

        // テストパターン画像を生成
        let mut img = RgbImage::new(VIDEO_WIDTH, VIDEO_HEIGHT);
        let bar_width = VIDEO_WIDTH / 7;

        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let bar_index = (x / bar_width).min(6) as usize;
            let (r, g, b) = colors[bar_index];

            // 下部にスクロールするバーを表示（動きがわかるように）
            if y > VIDEO_HEIGHT * 3 / 4 {
                let scroll = ((frame_count * 4 + x as u64) % VIDEO_WIDTH as u64) as u32;
                let brightness = if scroll < VIDEO_WIDTH / 2 { 255u8 } else { 0u8 };
                *pixel = image::Rgb([brightness, brightness, brightness]);
            } else {
                *pixel = image::Rgb([r, g, b]);
            }
        }

        // JPEG エンコード
        let mut jpeg_buf = Cursor::new(Vec::new());
        let encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, 80);
        img.write_with_encoder(encoder)?;
        let jpeg_data = jpeg_buf.into_inner();

        publisher.send_object(&jpeg_data).await?;
        tracing::debug!("sent frame #{}: {} bytes", frame_count, jpeg_data.len());
        frame_count += 1;
    }
}

async fn run_video_subscriber() -> Result<()> {
    use sdl2::event::Event;
    use sdl2::keyboard::Keycode;
    use sdl2::pixels::PixelFormatEnum;

    let connection = connect().await?;
    let (control_send, control_recv) = client_setup(&connection).await?;

    let subscriber = Subscriber::new(
        connection,
        control_send,
        control_recv,
        TRACK_NAMESPACE.to_string(),
        VIDEO_TRACK_NAME.to_string(),
    )
    .await?;

    tracing::info!("video subscriber ready. waiting for frames...");

    // SDL2 初期化
    let sdl_context = sdl2::init().map_err(|e| anyhow::anyhow!(e))?;
    let video_subsystem = sdl_context.video().map_err(|e| anyhow::anyhow!(e))?;

    let window = video_subsystem
        .window("MoQ Video", VIDEO_WIDTH, VIDEO_HEIGHT)
        .position_centered()
        .build()?;

    let mut canvas = window.into_canvas().build()?;
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator.create_texture_streaming(
        PixelFormatEnum::RGB24,
        VIDEO_WIDTH,
        VIDEO_HEIGHT,
    )?;

    let mut event_pump = sdl_context.event_pump().map_err(|e| anyhow::anyhow!(e))?;

    loop {
        // SDL2 イベント処理
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    tracing::info!("quitting video subscriber");
                    return Ok(());
                }
                _ => {}
            }
        }

        // フレーム受信（ノンブロッキングでタイムアウト付き）
        let recv_result = tokio::time::timeout(
            Duration::from_millis(50),
            subscriber.recv_object(),
        )
        .await;

        if let Ok(Ok((_header, payload))) = recv_result {
            // JPEG → RGB デコード
            match image::load_from_memory_with_format(&payload, image::ImageFormat::Jpeg) {
                Ok(img) => {
                    let rgb = img
                        .resize_exact(
                            VIDEO_WIDTH,
                            VIDEO_HEIGHT,
                            image::imageops::FilterType::Nearest,
                        )
                        .to_rgb8();
                    let pitch = VIDEO_WIDTH as usize * 3;
                    texture.update(None, rgb.as_raw(), pitch)?;
                    canvas.clear();
                    canvas.copy(&texture, None, None).map_err(|e| anyhow::anyhow!(e))?;
                    canvas.present();
                }
                Err(e) => {
                    tracing::warn!("jpeg decode error: {e}");
                }
            }
        }
    }
}

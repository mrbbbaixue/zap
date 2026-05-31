//! WinRT 实时语音识别 smoke test。
//!
//! 运行方式:
//! ```
//! cargo run -p voice_transcription --example realtime_smoke
//! ```
//!
//! 按 Ctrl+C 停止识别并退出。

#[cfg(target_os = "windows")]
use voice_transcription::{RealtimeSpeechRecognizer, RealtimeSpeechEvent};

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("此示例仅支持 Windows 平台");
}

#[cfg(target_os = "windows")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== WinRT 实时语音识别 Smoke Test ===");
    println!("正在初始化识别器...");

    let recognizer = RealtimeSpeechRecognizer::new()?;
    let session = recognizer.start_session().await?;

    println!("✓ 识别会话已启动，请说话...");
    println!("  (按 Ctrl+C 停止)");
    println!();

    let events = session.events();

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) => match event {
                        RealtimeSpeechEvent::Hypothesis { text } => {
                            println!("  [Hypothesis] {}", text);
                        }
                        RealtimeSpeechEvent::Final { text } => {
                            println!("  [Final]     {}", text);
                        }
                        RealtimeSpeechEvent::Completed => {
                            println!("  [Completed]");
                            break;
                        }
                        RealtimeSpeechEvent::Canceled => {
                            println!("  [Canceled]");
                            break;
                        }
                        RealtimeSpeechEvent::Error(err) => {
                            eprintln!("  [Error]     {}", err);
                            break;
                        }
                    },
                    Err(err) => {
                        eprintln!("Channel error: {}", err);
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n正在停止识别...");
                session.stop().await?;
                println!("✓ 识别已停止");
                break;
            }
        }
    }

    println!("\n=== Smoke Test 完成 ===");
    Ok(())
}

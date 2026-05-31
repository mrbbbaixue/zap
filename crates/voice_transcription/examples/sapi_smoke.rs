use std::path::PathBuf;

use voice_transcription::SystemSpeechRecognizer;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: sapi_smoke <wav-file>"))?;

    let recognizer = SystemSpeechRecognizer::new()?;
    let text = recognizer.transcribe_wav_file(path)?;
    println!("{text}");

    Ok(())
}

mod audio;
mod com;
mod event;
mod phrase;
mod recognizer;

pub use audio::AudioFormat;
pub use com::with_initialized_com;
pub use recognizer::Recognizer;

pub fn sapi<T>(operation: &'static str, result: windows_core::Result<T>) -> crate::Result<T> {
    result.map_err(|source| crate::Error::from_windows_operation(operation, source))
}

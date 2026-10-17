//! Chtra: a fast, small, memory-safe photo viewer and batch converter.
//!
//! The library holds everything that is not UI: format detection, decoding,
//! encoding, resizing, EXIF handling, folder listing and batch conversion.

#![forbid(unsafe_code)]

pub mod convert;
pub mod decode;
pub mod encode;
pub mod folder;
pub mod format;
pub mod metadata;
pub mod resize;

pub use decode::{DecodeOptions, Decoded, decode_file};
pub use format::Format;

/// Runs a codec call so that a bug in it fails this one file instead of taking
/// down the whole viewer or batch. Returns the panic message on failure.
fn isolate<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".into());
        format!("internal codec error: {msg}")
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn isolate_turns_a_panic_into_an_error() {
        assert_eq!(super::isolate(|| 7), Ok(7));
        let err = super::isolate(|| -> u8 { panic!("bad file") }).unwrap_err();
        assert!(err.contains("bad file"), "{err}");
    }
}

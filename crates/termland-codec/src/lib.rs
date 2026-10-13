pub mod encoder;
pub mod decoder;
pub mod audio;

pub use encoder::{VideoEncoder, EncoderBackend, EncoderConfig, EncodedFrame, RateTarget, probe_best_encoder};
pub use decoder::VideoDecoder;
pub use audio::{OpusEncoder, OpusDecoder};

/// Run `f` with FFmpeg's own logging silenced, unless debug logging is on.
///
/// Choosing a backend means trying each in turn, and FFmpeg prints errors for
/// every one the machine cannot open (no Intel GPU for QSV, no NVIDIA GPU for
/// NVENC, ...). Nothing is wrong, but it reads as if something were. Each
/// skipped backend is logged at debug here instead, and with debug logging on
/// FFmpeg's detail is left visible.
///
/// FFmpeg's log level is process-wide, so callers are serialised: two
/// overlapping calls could otherwise restore each other's silenced level and
/// leave FFmpeg quiet for good. A nested call on the same thread just runs.
pub(crate) fn quiet_ffmpeg<T>(f: impl FnOnce() -> T) -> T {
    use ffmpeg_next::util::log;
    use std::cell::Cell;
    use std::sync::Mutex;

    thread_local!(static QUIET: Cell<bool> = const { Cell::new(false) });
    static LOCK: Mutex<()> = Mutex::new(());

    if QUIET.get() || tracing::enabled!(tracing::Level::DEBUG) {
        return f();
    }
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Restores the level even if `f` panics.
    struct Restore(log::Level);
    impl Drop for Restore {
        fn drop(&mut self) {
            log::set_level(self.0);
            QUIET.set(false);
        }
    }
    let _restore = Restore(log::get_level().unwrap_or(log::Level::Info));
    log::set_level(log::Level::Quiet);
    QUIET.set(true);
    f()
}

#[cfg(test)]
mod quiet_ffmpeg_tests {
    use super::quiet_ffmpeg;
    use ffmpeg_next::util::log::{get_level, set_level, Level};

    // FFmpeg's level is process-wide, so these run as one test rather than
    // racing each other on parallel test threads.
    #[test]
    fn silences_ffmpeg_only_while_probing_and_only_without_debug_logging() {
        set_level(Level::Info);

        // No subscriber: debug is off, so FFmpeg is quiet inside and restored after.
        assert_eq!(quiet_ffmpeg(|| get_level()), Ok(Level::Quiet));
        assert_eq!(get_level(), Ok(Level::Info));

        // Nested calls run without deadlocking, and the outer one restores.
        assert_eq!(quiet_ffmpeg(|| quiet_ffmpeg(|| get_level())), Ok(Level::Quiet));
        assert_eq!(get_level(), Ok(Level::Info));

        // A panic inside still restores the level.
        let _ = std::panic::catch_unwind(|| quiet_ffmpeg(|| panic!("probe failed")));
        assert_eq!(get_level(), Ok(Level::Info));

        // With debug logging on, FFmpeg's detail is left alone.
        let debug = tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).finish();
        tracing::subscriber::with_default(debug, || {
            assert_eq!(quiet_ffmpeg(|| get_level()), Ok(Level::Info));
        });
    }
}

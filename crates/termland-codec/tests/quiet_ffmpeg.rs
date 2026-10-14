//! `quiet_ffmpeg` changes FFmpeg's process-wide log level, as do the encoder
//! and decoder tests that probe backends through it. In the library's unit
//! tests those run on parallel threads and this test could observe their
//! level, so it lives in its own test binary, which is its own process.

use ffmpeg_next::util::log::{get_level, set_level, Level};
use termland_codec::quiet_ffmpeg;

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

//! Standalone AV1 encode->decode test.
//! Run with: cargo run -p termland-codec --bin av1_test

use termland_codec::encoder::*;
use termland_codec::decoder::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let width = 1280;
    let height = 720;

    // Generate a test frame (RGBA gradient)
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            rgba[i] = (x * 255 / width) as u8;     // R
            rgba[i + 1] = (y * 255 / height) as u8; // G
            rgba[i + 2] = 128;                       // B
            rgba[i + 3] = 255;                       // A
        }
    }

    let config = EncoderConfig {
        width: width as u32,
        height: height as u32,
        fps: 30,
        bitrate_kbps: 8000,
        keyframe_interval: 30,
    };

    // Probe encoder
    let mut encoder = match probe_best_encoder(&config) {
        Ok(enc) => enc,
        Err(e) => {
            eprintln!("No encoder available: {e}");
            std::process::exit(1);
        }
    };

    println!("Encoder: {}", encoder.backend());
    println!("Sending 30 frames to fill encoder pipeline...");

    // Send multiple frames - HW encoders buffer
    let mut encoded_packets = Vec::new();
    for i in 0..30 {
        // Vary the frame slightly so the encoder doesn't skip
        let mut frame = rgba.clone();
        frame[0] = i as u8;

        match encoder.encode_frame(&frame, i * 33333, i == 0) {
            Ok(packets) => {
                if packets.is_empty() {
                    println!("  Frame {i}: encoder buffering (no output)");
                }
                for ef in packets {
                    println!(
                        "  Frame {i}: encoded {} bytes, keyframe={}",
                        ef.data.len(),
                        ef.keyframe
                    );
                    let hex: String = ef.data.iter().take(16)
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("    First bytes: {hex}");
                    encoded_packets.push(ef);
                }
            }
            Err(e) => {
                eprintln!("  Frame {i}: encode error: {e}");
            }
        }
    }

    // Flush remaining
    match encoder.flush() {
        Ok(flushed) => {
            for ef in flushed {
                if !ef.data.is_empty() {
                    println!("  Flushed: {} bytes, keyframe={}", ef.data.len(), ef.keyframe);
                    encoded_packets.push(ef);
                }
            }
        }
        Err(e) => eprintln!("Flush error: {e}"),
    }

    println!("\nTotal encoded packets: {}", encoded_packets.len());

    if encoded_packets.is_empty() {
        eprintln!("No encoded packets produced!");
        std::process::exit(1);
    }

    // Now try decoding
    println!("\nDecoding with dav1d...");
    let mut decoder = match Av1Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Decoder init failed: {e}");
            std::process::exit(1);
        }
    };

    let mut decoded_count = 0;
    for (i, packet) in encoded_packets.iter().enumerate() {
        match decoder.decode(&packet.data) {
            Ok((w, h, pixels)) => {
                decoded_count += 1;
                println!(
                    "  Packet {i}: decoded {}x{} ({} pixels)",
                    w, h, pixels.len()
                );
            }
            Err(DecoderError::NoFrame) => {
                println!("  Packet {i}: no frame yet (decoder buffering)");
            }
            Err(e) => {
                eprintln!("  Packet {i}: decode error: {e}");
            }
        }
    }

    println!("\nDecoded {decoded_count}/{} packets successfully", encoded_packets.len());

    if decoded_count > 0 {
        println!("AV1 encode->decode pipeline WORKS!");
    } else {
        println!("AV1 encode->decode pipeline FAILED - no frames decoded");
        std::process::exit(1);
    }
}

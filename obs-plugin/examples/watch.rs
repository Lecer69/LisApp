//! Decodes the camera stream with no OBS involved, and reports what arrives. For checking the PC side
//! on its own: if this is smooth and OBS isn't, the problem is in OBS or the plugin's output.

use lis_camera::decoder::Decoder;
use lis_video::{read_packet, Packet};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|text| text.parse().ok()).unwrap_or(10);
    let dir = std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let path = PathBuf::from(dir).join("lis-camera.sock");
    let stream = UnixStream::connect(&path).expect("lis-daemon's camera socket; is it running?");
    let mut reader = BufReader::with_capacity(256 * 1024, stream);

    let mut decoder = Decoder::new().expect("an H.264 decoder");
    let mut started = None;
    let (mut frames, mut bytes, mut decoded) = (0u64, 0u64, 0u64);
    let mut decode_time = Duration::ZERO;
    let mut first_pts = 0i64;
    let mut worst_lag = Duration::ZERO;
    let mut size = (0, 0);

    loop {
        match read_packet(&mut reader).expect("the stream") {
            Packet::Info(info) => println!("camera: {info:?}"),
            Packet::Frame(frame) => {
                let begin = started.get_or_insert_with(|| {
                    first_pts = frame.pts_us;
                    Instant::now()
                });
                let expected = Duration::from_micros((frame.pts_us - first_pts).max(0) as u64);
                worst_lag = worst_lag.max(begin.elapsed().saturating_sub(expected));

                frames += 1;
                bytes += frame.data.len() as u64;
                let timer = Instant::now();
                decoder
                    .decode(&frame.data, frame.pts_us, |out| {
                        decoded += 1;
                        size = (out.width, out.height);
                    })
                    .expect("decoding");
                decode_time += timer.elapsed();

                if begin.elapsed() >= Duration::from_secs(seconds) {
                    break;
                }
            }
            Packet::End => {
                println!("the camera stopped");
                break;
            }
            Packet::Viewers(_) => {}
        }
    }

    let elapsed = started.expect("no frames arrived").elapsed().as_secs_f64();
    println!("{}x{}: {frames} frames, {decoded} decoded", size.0, size.1);
    println!("{:.1} fps, {:.1} Mbps", frames as f64 / elapsed, bytes as f64 * 8.0 / 1e6 / elapsed);
    println!("decoding took {:.1} ms a frame", decode_time.as_secs_f64() * 1000.0 / decoded.max(1) as f64);
    println!("worst lag behind the stream: {} ms", worst_lag.as_millis());
}

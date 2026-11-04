//! A test video made the way the phone's encoder makes them: H.264 with no B-frames, one keyframe a
//! second, and SPS and PPS before each keyframe.

use std::path::Path;
use std::process::{Command, Stdio};

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const FPS: u32 = 60;

pub fn make_video(path: &Path, seconds: u32) {
    let size = format!("testsrc2=s={WIDTH}x{HEIGHT}:r={FPS}");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-loglevel", "error", "-f", "lavfi", "-i", &size, "-t", &seconds.to_string()])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-bf", "0", "-g", &FPS.to_string()])
        .args(["-pix_fmt", "yuv420p", "-x264-params", "aud=1:repeat-headers=1", "-f", "h264"])
        .arg(path)
        .stdout(Stdio::null())
        .status()
        .expect("ffmpeg, to make a test video");
    assert!(status.success(), "ffmpeg failed");
}

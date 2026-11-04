#[path = "common.rs"]
mod common;

use common::{make_video, FPS, HEIGHT, WIDTH};
use lis_camera::decoder::Decoder;
use lis_camera::sys::*;
use lis_video::h264::split_access_units;

#[test]
fn every_frame_comes_out_at_once_and_in_the_right_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.h264");
    make_video(&path, 2);
    let data = std::fs::read(&path).unwrap();
    let units = split_access_units(&data);
    assert_eq!(units.len() as u32, 2 * FPS);
    assert!(units[0].keyframe);
    assert_eq!(units.iter().filter(|unit| unit.keyframe).count(), 2);

    let mut decoder = Decoder::new().expect("a decoder");
    let mut decoded = 0;
    let mut late = 0;
    for (index, unit) in units.iter().enumerate() {
        let mut this_time = 0;
        decoder
            .decode(unit.data, index as i64, |frame| {
                this_time += 1;
                assert_eq!(frame.width as u32, WIDTH);
                assert_eq!(frame.height as u32, HEIGHT);
                assert_eq!(frame.format, AVPixelFormat_AV_PIX_FMT_YUV420P);
                assert!(!frame.data[0].is_null() && !frame.data[1].is_null() && !frame.data[2].is_null());
                assert_eq!(frame.pts, index as i64, "a frame came out late");
            })
            .expect("decoding");
        if this_time == 0 {
            late += 1;
        }
        decoded += this_time;
    }
    assert_eq!(decoded, units.len(), "every frame decodes");
    assert_eq!(late, 0, "no frame may be held back, which would be delay");
}

#[test]
fn joining_at_a_later_keyframe_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.h264");
    make_video(&path, 2);
    let data = std::fs::read(&path).unwrap();
    let units = split_access_units(&data);
    let second_key = units.iter().rposition(|unit| unit.keyframe).unwrap();
    assert!(second_key > 0);

    let mut decoder = Decoder::new().expect("a decoder");
    let mut decoded = 0;
    for unit in &units[second_key..] {
        decoder.decode(unit.data, 0, |_| decoded += 1).expect("decoding");
    }
    assert_eq!(decoded, units.len() - second_key);
}

#[test]
fn garbage_does_not_crash_the_decoder() {
    let mut decoder = Decoder::new().expect("a decoder");
    let mut decoded = 0;
    for bytes in [&b""[..], &[0u8; 32], &[0, 0, 0, 1, 0x65, 1, 2, 3], &[0xff; 4096]] {
        let _ = decoder.decode(bytes, 0, |_| decoded += 1);
    }
    assert_eq!(decoded, 0);
}

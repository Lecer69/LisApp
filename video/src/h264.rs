//! Splits a raw H.264 file into frames, so a recording can stand in for the phone's camera.

const NAL_IDR: u8 = 5;
const NAL_AUD: u8 = 9;

#[derive(Debug, PartialEq)]
pub struct AccessUnit<'a> {
    pub data: &'a [u8],
    pub keyframe: bool,
}

/// Where each NAL unit's start code begins, and the byte after that start code.
fn start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let begin = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            found.push((begin, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    found
}

/// Needs access unit delimiters, as `ffmpeg ... -x264-params aud=1` writes.
pub fn split_access_units(data: &[u8]) -> Vec<AccessUnit<'_>> {
    let nals = start_codes(data);
    let mut units = Vec::new();
    let mut begin: Option<usize> = None;
    let mut keyframe = false;

    for &(start, body) in &nals {
        let kind = data.get(body).map_or(0, |byte| byte & 0x1f);
        if kind == NAL_AUD {
            if let Some(first) = begin {
                units.push(AccessUnit { data: &data[first..start], keyframe });
            }
            begin = Some(start);
            keyframe = false;
        } else if begin.is_none() {
            begin = Some(start);
        }
        if kind == NAL_IDR {
            keyframe = true;
        }
    }
    if let Some(first) = begin {
        units.push(AccessUnit { data: &data[first..], keyframe });
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_delimiters_and_spots_keyframes() {
        let stream: &[u8] = &[
            0, 0, 0, 1, 0x09, 0x10, // delimiter
            0, 0, 0, 1, 0x67, 0xaa, // SPS
            0, 0, 1, 0x65, 0xbb, // IDR
            0, 0, 0, 1, 0x09, 0x30, // delimiter
            0, 0, 1, 0x41, 0xcc, // P frame
        ];
        let units = split_access_units(stream);
        assert_eq!(units.len(), 2);
        assert!(units[0].keyframe);
        assert!(!units[1].keyframe);
        assert_eq!(units[0].data, &stream[..17]);
        assert_eq!(units[1].data, &stream[17..]);
    }

    #[test]
    fn nothing_recognisable_gives_no_frames() {
        assert!(split_access_units(&[]).is_empty());
        assert!(split_access_units(&[1, 2, 3, 4]).is_empty());
    }
}

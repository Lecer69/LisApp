//! H.264 to YUV frames with FFmpeg, tuned to give each frame back as soon as it's decoded.

use crate::sys::*;
use std::ptr;

// AVERROR(EAGAIN), a macro bindgen can't see: the decoder wants more data
const AGAIN: i32 = -11;

pub struct Decoder {
    context: *mut AVCodecContext,
    packet: *mut AVPacket,
    frame: *mut AVFrame,
}

// Only ever used from one thread at a time
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Option<Self> {
        unsafe {
            let codec = avcodec_find_decoder(AVCodecID_AV_CODEC_ID_H264);
            if codec.is_null() {
                return None;
            }
            let mut decoder = Self { context: avcodec_alloc_context3(codec), packet: av_packet_alloc(), frame: av_frame_alloc() };
            if decoder.context.is_null() || decoder.packet.is_null() || decoder.frame.is_null() {
                return None;
            }
            // Frame threading would hold frames back, which is delay
            (*decoder.context).thread_type = FF_THREAD_SLICE as i32;
            (*decoder.context).thread_count = 0;
            (*decoder.context).flags |= AV_CODEC_FLAG_LOW_DELAY as i32;
            if avcodec_open2(decoder.context, codec, ptr::null_mut()) < 0 {
                return None;
            }
            let _ = &mut decoder;
            Some(decoder)
        }
    }

    /// Calls `output` for each finished frame. An error means waiting for the next keyframe.
    pub fn decode(&mut self, data: &[u8], pts_us: i64, mut output: impl FnMut(&AVFrame)) -> Result<(), i32> {
        unsafe {
            let result = av_new_packet(self.packet, data.len() as i32);
            if result < 0 {
                return Err(result);
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (*self.packet).data, data.len());
            (*self.packet).pts = pts_us;

            let sent = avcodec_send_packet(self.context, self.packet);
            av_packet_unref(self.packet);
            if sent < 0 && sent != AGAIN {
                return Err(sent);
            }

            loop {
                let received = avcodec_receive_frame(self.context, self.frame);
                if received == AGAIN {
                    return Ok(());
                }
                if received < 0 {
                    return Err(received);
                }
                output(&*self.frame);
                av_frame_unref(self.frame);
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            avcodec_free_context(&mut self.context);
            av_packet_free(&mut self.packet);
            av_frame_free(&mut self.frame);
        }
    }
}

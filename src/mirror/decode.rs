//! H.264 to RGBA pictures, in process.
//!
//! The phone's encoder output is decoded with OpenH264 (BSD-2-Clause, built
//! from its source by the `openh264` crate). scrcpy sends the codec
//! configuration (SPS and PPS) as its own packet ahead of the pictures.

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

use crate::error::{Error, Result};

/// One decoded picture, straight RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub struct VideoDecoder {
    inner: Decoder,
}

impl VideoDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self { inner: Decoder::new().map_err(|e| Error::Invalid(format!("could not start the H.264 decoder: {e}")))? })
    }

    /// Feeds one packet. `None` for configuration packets and for pictures the
    /// decoder is still waiting to complete.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Option<Frame>> {
        self.decode_picture(packet, true)
    }

    /// Like [`decode`](Self::decode); with `convert` false the picture is decoded
    /// (later frames depend on it) but not converted to RGBA, which is most of
    /// the work and pointless while nobody is looking.
    pub fn decode_picture(&mut self, packet: &[u8], convert: bool) -> Result<Option<Frame>> {
        let yuv = self.inner.decode(packet).map_err(|e| Error::parse("H.264 video", e.to_string()))?;
        Ok(yuv.filter(|_| convert).map(|yuv| {
            let (width, height) = yuv.dimensions();
            let mut rgba = vec![0u8; width * height * 4];
            yuv.write_rgba8(&mut rgba);
            Frame { width, height, rgba }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 frames of 320x240 test video, baseline profile, as Annex B.
    const STREAM: &[u8] = include_bytes!("../../tests/fixtures/test-320x240.h264");

    /// Splits like the scrcpy server: SPS+PPS as one configuration packet, then one packet per picture.
    pub(crate) fn packets() -> Vec<(bool, Vec<u8>)> {
        let mut out: Vec<(bool, Vec<u8>)> = Vec::new();
        let mut config = Vec::new();
        for nal in openh264::nal_units(STREAM) {
            let kind = nal.iter().position(|&b| b == 1).map(|i| nal[i + 1] & 0x1f).unwrap_or(0);
            if kind == 7 || kind == 8 {
                config.extend_from_slice(nal);
            } else {
                if !config.is_empty() {
                    out.push((true, std::mem::take(&mut config)));
                }
                out.push((false, nal.to_vec()));
            }
        }
        out
    }

    #[test]
    fn decodes_every_picture_of_a_stream() {
        let mut decoder = VideoDecoder::new().unwrap();
        let mut frames = Vec::new();
        for (config, data) in packets() {
            let frame = decoder.decode(&data).unwrap();
            if config {
                assert!(frame.is_none(), "configuration is not a picture");
            }
            frames.extend(frame);
        }
        assert_eq!(frames.len(), 10);
        assert!(frames.iter().all(|f| (f.width, f.height) == (320, 240) && f.rgba.len() == 320 * 240 * 4));
        assert!(frames[0].rgba.chunks(4).all(|px| px[3] == 255), "opaque");
        assert!(frames[0].rgba.iter().any(|&b| b != 0), "not a black frame");
    }

    #[test]
    fn garbage_is_an_error_or_nothing_never_a_panic() {
        let mut decoder = VideoDecoder::new().unwrap();
        for junk in [&b""[..], b"\x00\x00\x00\x01\x65garbage", b"not video at all", &[0xff; 64]] {
            let _ = decoder.decode(junk);
        }
        // The decoder still works afterwards.
        let mut n = 0;
        for (_, data) in packets() {
            n += usize::from(decoder.decode(&data).ok().flatten().is_some());
        }
        assert!(n >= 8, "decoded {n} pictures after bad input");
    }
}

//! The scrcpy 3.1 wire protocol, client side.
//!
//! Written against the server source of tag `v3.1` (`DesktopConnection`,
//! `Streamer`, `ControlMessageReader`, `DeviceMessageWriter`, `Options`). The
//! protocol is internal to scrcpy and changes between versions, which is why the
//! matching server jar is bundled. Everything here is pure so it can be tested
//! with hand-computed bytes.

use crate::error::{Error, Result};

/// Must equal the bundled server's version exactly or it refuses to start.
pub const SERVER_VERSION: &str = "3.1";
pub const SERVER_CLASS: &str = "com.genymobile.scrcpy.Server";
/// Length of the NUL-padded device name sent first on the first socket.
pub const DEVICE_NAME_LEN: usize = 64;
/// `"h264"` as a big-endian integer.
pub const CODEC_H264: u32 = 0x6832_3634;
/// Upper bound for one video packet; larger sizes mean the stream is out of sync.
pub const MAX_PACKET: usize = 16 * 1024 * 1024;
/// scrcpy refuses text injections longer than this many bytes.
pub const TEXT_MAX: usize = 300;
/// Pointer id the server treats as "the mouse" (`-1`).
pub const POINTER_MOUSE: u64 = u64::MAX;

/// Options of one server run.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerOptions {
    /// 31-bit session id; names the abstract socket `scrcpy_<scid as %08x>`.
    pub scid: u32,
    pub bit_rate: u32,
    /// Longest side in pixels, 0 for the device's own size.
    pub max_size: u32,
    /// 0 for the device's default.
    pub max_fps: u32,
    pub control: bool,
    pub stay_awake: bool,
    pub show_touches: bool,
    pub power_off_on_close: bool,
    pub clipboard_autosync: bool,
}

impl ServerOptions {
    /// Name of the abstract socket the server connects to (reverse tunnel).
    pub fn socket_name(&self) -> String {
        format!("scrcpy_{:08x}", self.scid)
    }

    /// Arguments after the class name: the version first, then `key=value` pairs.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            SERVER_VERSION.to_string(),
            format!("scid={:08x}", self.scid),
            "log_level=info".into(),
            "video=true".into(),
            "audio=false".into(),
            format!("control={}", self.control),
            "video_codec=h264".into(),
            // The device connects to us (reverse), so no dummy byte is sent.
            "tunnel_forward=false".into(),
            "send_device_meta=true".into(),
            "send_frame_meta=true".into(),
            "send_codec_meta=true".into(),
            // Let the server restore settings (stay awake, touches) when it ends.
            "cleanup=true".into(),
            format!("video_bit_rate={}", self.bit_rate.max(100_000)),
            format!("stay_awake={}", self.stay_awake),
            format!("show_touches={}", self.show_touches),
            format!("power_off_on_close={}", self.power_off_on_close),
            format!("clipboard_autosync={}", self.clipboard_autosync),
        ];
        if self.max_size > 0 {
            args.push(format!("max_size={}", self.max_size));
        }
        if self.max_fps > 0 {
            args.push(format!("max_fps={}", self.max_fps));
        }
        args
    }
}

/// The 12 bytes that follow the device name on the video socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub codec: u32,
    pub width: u32,
    pub height: u32,
}

impl VideoHeader {
    pub const LEN: usize = 12;

    pub fn parse(bytes: &[u8; Self::LEN]) -> Result<VideoHeader> {
        let word = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let header = VideoHeader { codec: word(0), width: word(4), height: word(8) };
        if header.codec != CODEC_H264 {
            return Err(Error::Invalid(format!("the device sent video codec {:#010x}, not H.264", header.codec)));
        }
        Ok(header)
    }
}

/// The device name from the first 64 bytes: up to the first NUL.
pub fn parse_device_name(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Header in front of every video packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Codec configuration (SPS/PPS) rather than a picture.
    pub config: bool,
    pub key_frame: bool,
    /// Presentation time in microseconds.
    pub pts: u64,
    pub size: usize,
}

impl PacketHeader {
    pub const LEN: usize = 12;
    const CONFIG: u64 = 1 << 63;
    const KEY: u64 = 1 << 62;

    pub fn parse(bytes: &[u8; Self::LEN]) -> Result<PacketHeader> {
        let mut flags = [0u8; 8];
        flags.copy_from_slice(&bytes[..8]);
        let flags = u64::from_be_bytes(flags);
        let size = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        if size > MAX_PACKET {
            return Err(Error::parse("scrcpy video", format!("packet of {size} bytes")));
        }
        Ok(PacketHeader {
            config: flags & Self::CONFIG != 0,
            key_frame: flags & Self::KEY != 0,
            pts: flags & !(Self::CONFIG | Self::KEY),
            size,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Down = 0,
    Up = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Down = 0,
    Up = 1,
    Move = 2,
}

/// Android `MotionEvent.BUTTON_*` values the client sends.
pub mod button {
    pub const PRIMARY: u32 = 1;
}

/// Android key codes used by the toolbar and keyboard forwarding.
pub mod key {
    pub const HOME: u32 = 3;
    pub const BACK: u32 = 4;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const VOLUME_UP: u32 = 24;
    pub const VOLUME_DOWN: u32 = 25;
    pub const POWER: u32 = 26;
    pub const TAB: u32 = 61;
    pub const SPACE: u32 = 62;
    pub const ENTER: u32 = 66;
    pub const DEL: u32 = 67;
    pub const MENU: u32 = 82;
    pub const PAGE_UP: u32 = 92;
    pub const PAGE_DOWN: u32 = 93;
    pub const ESCAPE: u32 = 111;
    pub const FORWARD_DEL: u32 = 112;
    pub const MOVE_HOME: u32 = 122;
    pub const MOVE_END: u32 = 123;
    pub const VOLUME_MUTE: u32 = 164;
    pub const APP_SWITCH: u32 = 187;
}

/// A message from the client to the device (`ControlMessage`).
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    Key {
        action: KeyAction,
        keycode: u32,
        repeat: u32,
        meta: u32,
    },
    Text(String),
    Touch {
        action: TouchAction,
        pointer: u64,
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        pressure: f32,
        action_button: u32,
        buttons: u32,
    },
    Scroll {
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        h: f32,
        v: f32,
        buttons: u32,
    },
    /// Back, or wake the screen when it is off. `action` is a [`KeyAction`].
    BackOrScreenOn(KeyAction),
    ExpandNotifications,
    ExpandSettings,
    CollapsePanels,
    GetClipboard {
        cut: bool,
    },
    SetClipboard {
        sequence: u64,
        paste: bool,
        text: String,
    },
    SetDisplayPower(bool),
    Rotate,
    /// Ask for a fresh key frame (after dropped packets).
    ResetVideo,
}

/// `0.0..=1.0` as an unsigned 16-bit fixed-point number (`0xffff` is exactly 1).
fn u16_fixed(value: f32) -> u16 {
    let v = value.clamp(0.0, 1.0);
    if v >= 1.0 {
        0xffff
    } else {
        (v * 65536.0) as u16
    }
}

/// `-1.0..=1.0` as a signed 16-bit fixed-point number (`0x7fff` is exactly 1).
fn i16_fixed(value: f32) -> i16 {
    let v = value.clamp(-1.0, 1.0);
    if v >= 1.0 {
        0x7fff
    } else {
        (v * 32768.0) as i16
    }
}

/// Longest prefix of `text` that fits in `max` bytes without cutting a character.
fn utf8_prefix(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Control {
    // Type ids from `ControlMessage.TYPE_*`.
    const KEYCODE: u8 = 0;
    const TEXT: u8 = 1;
    const TOUCH: u8 = 2;
    const SCROLL: u8 = 3;
    const BACK_OR_SCREEN_ON: u8 = 4;
    const EXPAND_NOTIFICATIONS: u8 = 5;
    const EXPAND_SETTINGS: u8 = 6;
    const COLLAPSE_PANELS: u8 = 7;
    const GET_CLIPBOARD: u8 = 8;
    const SET_CLIPBOARD: u8 = 9;
    const SET_DISPLAY_POWER: u8 = 10;
    const ROTATE: u8 = 11;
    const RESET_VIDEO: u8 = 17;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        match self {
            Control::Key { action, keycode, repeat, meta } => {
                out.push(Self::KEYCODE);
                out.push(*action as u8);
                out.extend(keycode.to_be_bytes());
                out.extend(repeat.to_be_bytes());
                out.extend(meta.to_be_bytes());
            }
            Control::Text(text) => {
                let text = utf8_prefix(text, TEXT_MAX);
                out.push(Self::TEXT);
                out.extend((text.len() as u32).to_be_bytes());
                out.extend(text.as_bytes());
            }
            Control::Touch { action, pointer, x, y, width, height, pressure, action_button, buttons } => {
                out.push(Self::TOUCH);
                out.push(*action as u8);
                out.extend(pointer.to_be_bytes());
                push_position(&mut out, *x, *y, *width, *height);
                out.extend(u16_fixed(*pressure).to_be_bytes());
                out.extend(action_button.to_be_bytes());
                out.extend(buttons.to_be_bytes());
            }
            Control::Scroll { x, y, width, height, h, v, buttons } => {
                out.push(Self::SCROLL);
                push_position(&mut out, *x, *y, *width, *height);
                out.extend(i16_fixed(*h).to_be_bytes());
                out.extend(i16_fixed(*v).to_be_bytes());
                out.extend(buttons.to_be_bytes());
            }
            Control::BackOrScreenOn(action) => out.extend([Self::BACK_OR_SCREEN_ON, *action as u8]),
            Control::ExpandNotifications => out.push(Self::EXPAND_NOTIFICATIONS),
            Control::ExpandSettings => out.push(Self::EXPAND_SETTINGS),
            Control::CollapsePanels => out.push(Self::COLLAPSE_PANELS),
            Control::GetClipboard { cut } => out.extend([Self::GET_CLIPBOARD, if *cut { 2 } else { 1 }]),
            Control::SetClipboard { sequence, paste, text } => {
                // 5 bytes of the 256 KiB message limit are the type and the length.
                let text = utf8_prefix(text, (1 << 18) - 14);
                out.push(Self::SET_CLIPBOARD);
                out.extend(sequence.to_be_bytes());
                out.push(u8::from(*paste));
                out.extend((text.len() as u32).to_be_bytes());
                out.extend(text.as_bytes());
            }
            Control::SetDisplayPower(on) => out.extend([Self::SET_DISPLAY_POWER, u8::from(*on)]),
            Control::Rotate => out.push(Self::ROTATE),
            Control::ResetVideo => out.push(Self::RESET_VIDEO),
        }
        out
    }
}

fn push_position(out: &mut Vec<u8>, x: i32, y: i32, width: u16, height: u16) {
    out.extend(x.to_be_bytes());
    out.extend(y.to_be_bytes());
    out.extend(width.to_be_bytes());
    out.extend(height.to_be_bytes());
}

/// A message from the device (`DeviceMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceMessage {
    Clipboard(String),
    AckClipboard(u64),
    /// Output of a virtual HID device; ignored.
    UhidOutput,
}

/// Parses one message from the front of `buf`: `Ok(None)` when more bytes are
/// needed, otherwise the message and the number of bytes it used.
pub fn parse_device_message(buf: &[u8]) -> Result<Option<(DeviceMessage, usize)>> {
    let Some(&kind) = buf.first() else { return Ok(None) };
    match kind {
        0 => {
            if buf.len() < 5 {
                return Ok(None);
            }
            let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
            if len > 1 << 18 {
                return Err(Error::parse("scrcpy control", format!("clipboard of {len} bytes")));
            }
            if buf.len() < 5 + len {
                return Ok(None);
            }
            Ok(Some((DeviceMessage::Clipboard(String::from_utf8_lossy(&buf[5..5 + len]).into_owned()), 5 + len)))
        }
        1 => {
            if buf.len() < 9 {
                return Ok(None);
            }
            let mut seq = [0u8; 8];
            seq.copy_from_slice(&buf[1..9]);
            Ok(Some((DeviceMessage::AckClipboard(u64::from_be_bytes(seq)), 9)))
        }
        2 => {
            if buf.len() < 5 {
                return Ok(None);
            }
            let len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
            if buf.len() < 5 + len {
                return Ok(None);
            }
            Ok(Some((DeviceMessage::UhidOutput, 5 + len)))
        }
        other => Err(Error::parse("scrcpy control", format!("unknown device message {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ServerOptions {
        ServerOptions {
            scid: 0x1234abcd,
            bit_rate: 8_000_000,
            max_size: 1280,
            max_fps: 0,
            control: true,
            stay_awake: true,
            show_touches: false,
            power_off_on_close: false,
            clipboard_autosync: true,
        }
    }

    #[test]
    fn server_arguments_start_with_the_version_and_name_the_socket() {
        let o = opts();
        let args = o.args();
        assert_eq!(args[0], "3.1");
        assert!(args.contains(&"scid=1234abcd".to_string()));
        assert_eq!(o.socket_name(), "scrcpy_1234abcd");
        for expected in
            ["audio=false", "tunnel_forward=false", "video_codec=h264", "max_size=1280", "video_bit_rate=8000000", "stay_awake=true"]
        {
            assert!(args.contains(&expected.to_string()), "{expected}");
        }
        assert!(!args.iter().any(|a| a.starts_with("max_fps")), "unset limits are not sent");
        assert!(args[1..].iter().all(|a| a.contains('=')), "every option is key=value");
    }

    #[test]
    fn small_scids_are_zero_padded_like_the_server_formats_them() {
        let o = ServerOptions { scid: 0xab, ..opts() };
        assert_eq!(o.socket_name(), "scrcpy_000000ab");
        assert!(o.args().contains(&"scid=000000ab".to_string()));
    }

    #[test]
    fn video_header_needs_h264() {
        let mut raw = [0u8; 12];
        raw[..4].copy_from_slice(b"h264");
        raw[4..8].copy_from_slice(&1080u32.to_be_bytes());
        raw[8..].copy_from_slice(&2400u32.to_be_bytes());
        assert_eq!(VideoHeader::parse(&raw).unwrap(), VideoHeader { codec: CODEC_H264, width: 1080, height: 2400 });
        raw[..4].copy_from_slice(b"h265");
        assert!(VideoHeader::parse(&raw).is_err());
    }

    #[test]
    fn device_name_stops_at_the_first_nul() {
        let mut raw = [0u8; 64];
        raw[..8].copy_from_slice(b"Pixel 7\0");
        raw[9] = b'x';
        assert_eq!(parse_device_name(&raw), "Pixel 7");
        assert_eq!(parse_device_name(b"abc"), "abc");
    }

    #[test]
    fn packet_headers_split_flags_pts_and_size() {
        let mut raw = [0u8; 12];
        raw[..8].copy_from_slice(&(PacketHeader::CONFIG).to_be_bytes());
        raw[8..].copy_from_slice(&27u32.to_be_bytes());
        assert_eq!(PacketHeader::parse(&raw).unwrap(), PacketHeader { config: true, key_frame: false, pts: 0, size: 27 });
        raw[..8].copy_from_slice(&(PacketHeader::KEY | 123_456).to_be_bytes());
        let h = PacketHeader::parse(&raw).unwrap();
        assert_eq!((h.config, h.key_frame, h.pts), (false, true, 123_456));
        raw[8..].copy_from_slice(&(MAX_PACKET as u32 + 1).to_be_bytes());
        assert!(PacketHeader::parse(&raw).is_err(), "a packet bigger than any frame means the stream is out of sync");
    }

    #[test]
    fn touch_matches_the_layout_the_server_reads() {
        // type, action, pointer id (8), x, y (4 each), width, height (2 each), pressure (2), action button, buttons
        let msg = Control::Touch {
            action: TouchAction::Down,
            pointer: POINTER_MOUSE,
            x: 540,
            y: 1200,
            width: 1080,
            height: 2400,
            pressure: 1.0,
            action_button: button::PRIMARY,
            buttons: button::PRIMARY,
        };
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 1 + 1 + 8 + 12 + 2 + 8);
        assert_eq!(&bytes[..2], &[2, 0]);
        assert_eq!(&bytes[2..10], &[0xff; 8]);
        assert_eq!(&bytes[10..14], &540i32.to_be_bytes());
        assert_eq!(&bytes[14..18], &1200i32.to_be_bytes());
        assert_eq!(&bytes[18..22], &[0x04, 0x38, 0x09, 0x60]);
        assert_eq!(&bytes[22..24], &[0xff, 0xff], "full pressure is 0xffff");
        assert_eq!(&bytes[24..28], &1u32.to_be_bytes());
    }

    #[test]
    fn fixed_point_values_round_correctly() {
        assert_eq!(u16_fixed(0.0), 0);
        assert_eq!(u16_fixed(0.5), 0x8000);
        assert_eq!(u16_fixed(1.0), 0xffff);
        assert_eq!(u16_fixed(7.0), 0xffff);
        assert_eq!(u16_fixed(-1.0), 0);
        assert_eq!(i16_fixed(0.0), 0);
        assert_eq!(i16_fixed(-0.5), -0x4000);
        assert_eq!(i16_fixed(1.0), 0x7fff);
        assert_eq!(i16_fixed(-1.0), -0x8000);
        assert_eq!(i16_fixed(9.0), 0x7fff);
    }

    #[test]
    fn keys_and_simple_messages_encode_as_the_server_parses_them() {
        assert_eq!(
            Control::Key { action: KeyAction::Up, keycode: key::HOME, repeat: 0, meta: 0 }.encode(),
            vec![0, 1, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(Control::BackOrScreenOn(KeyAction::Down).encode(), vec![4, 0]);
        assert_eq!(Control::ExpandNotifications.encode(), vec![5]);
        assert_eq!(Control::ExpandSettings.encode(), vec![6]);
        assert_eq!(Control::CollapsePanels.encode(), vec![7]);
        assert_eq!(Control::GetClipboard { cut: false }.encode(), vec![8, 1]);
        assert_eq!(Control::SetDisplayPower(false).encode(), vec![10, 0]);
        assert_eq!(Control::Rotate.encode(), vec![11]);
        assert_eq!(Control::ResetVideo.encode(), vec![17]);
    }

    #[test]
    fn scroll_and_clipboard_layouts() {
        let s = Control::Scroll { x: 1, y: 2, width: 3, height: 4, h: 0.0, v: -1.0, buttons: 0 }.encode();
        assert_eq!(s.len(), 1 + 12 + 2 + 2 + 4);
        assert_eq!(&s[13..17], &[0, 0, 0x80, 0], "v scroll of -1 is 0x8000");
        let c = Control::SetClipboard { sequence: 7, paste: true, text: "héé".into() }.encode();
        assert_eq!(&c[..10], &[9, 0, 0, 0, 0, 0, 0, 0, 7, 1]);
        assert_eq!(&c[10..14], &5u32.to_be_bytes(), "the length is in bytes, not characters");
        assert_eq!(&c[14..], "héé".as_bytes());
    }

    #[test]
    fn text_is_cut_on_a_character_boundary() {
        let long = "é".repeat(400); // 800 bytes
        let bytes = Control::Text(long).encode();
        let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        assert_eq!(len, 300);
        assert_eq!(bytes.len(), 5 + len);
        assert!(std::str::from_utf8(&bytes[5..]).is_ok());
        assert_eq!(utf8_prefix("aé", 2), "a", "a character that does not fit is dropped whole");
    }

    #[test]
    fn device_messages_parse_incrementally() {
        let mut wire = vec![0u8];
        wire.extend(5u32.to_be_bytes());
        wire.extend(b"hello");
        wire.push(1);
        wire.extend(42u64.to_be_bytes());
        // Every strict prefix of the first message asks for more data.
        for cut in 0..10 {
            assert_eq!(parse_device_message(&wire[..cut]).unwrap(), None, "cut at {cut}");
        }
        let (first, used) = parse_device_message(&wire).unwrap().unwrap();
        assert_eq!((first, used), (DeviceMessage::Clipboard("hello".into()), 10));
        let (second, used) = parse_device_message(&wire[10..]).unwrap().unwrap();
        assert_eq!((second, used), (DeviceMessage::AckClipboard(42), 9));
        assert!(parse_device_message(&[9]).is_err());
        let mut huge = vec![0u8];
        huge.extend(u32::MAX.to_be_bytes());
        assert!(parse_device_message(&huge).is_err());
    }
}

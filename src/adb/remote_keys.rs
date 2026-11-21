//! Android key codes used by the remote controller and the key helpers.

use crate::adb::DeviceAdb;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key(pub u32);

impl Key {
    pub const HOME: Key = Key(3);
    pub const BACK: Key = Key(4);
    pub const DIGIT_0: Key = Key(7);
    pub const VOLUME_UP: Key = Key(24);
    pub const VOLUME_DOWN: Key = Key(25);
    pub const POWER: Key = Key(26);
    pub const DPAD_UP: Key = Key(19);
    pub const DPAD_DOWN: Key = Key(20);
    pub const DPAD_LEFT: Key = Key(21);
    pub const DPAD_RIGHT: Key = Key(22);
    pub const DPAD_CENTER: Key = Key(23);
    pub const MENU: Key = Key(82);
    pub const MUTE: Key = Key(164);
    pub const CHANNEL_UP: Key = Key(166);
    pub const CHANNEL_DOWN: Key = Key(167);
    pub const GUIDE: Key = Key(172);
    pub const SETTINGS: Key = Key(176);
    pub const TV_INPUT: Key = Key(178);
    pub const APP_SWITCH: Key = Key(187);

    /// Keys 1-9 follow 0 consecutively in Android's key map.
    pub fn digit(n: u32) -> Key {
        Key(Self::DIGIT_0.0 + n.min(9))
    }
}

impl DeviceAdb {
    pub async fn press(&self, key: Key) -> Result<()> {
        self.shell_ok(&format!("input keyevent {}", key.0)).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_map_to_android_codes() {
        assert_eq!(Key::digit(0), Key(7));
        assert_eq!(Key::digit(1), Key(8));
        assert_eq!(Key::digit(9), Key(16));
        assert_eq!(Key::digit(12), Key(16));
    }
}

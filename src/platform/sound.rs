//! Audible alerts, played through `NSSound`. Names refer to the system sounds
//! in `/System/Library/Sounds` (e.g. "Tink", "Glass", "Ping").

use objc2_app_kit::NSSound;
use objc2_foundation::NSString;

/// Play a named system sound. Silently does nothing if the name is unknown.
pub fn play(name: &str) {
    if name.is_empty() {
        return;
    }
    let ns = NSString::from_str(name);
    if let Some(sound) = NSSound::soundNamed(&ns) {
        sound.play();
    }
}

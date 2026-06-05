use objc2_app_kit::NSSound;
use objc2_foundation::NSString;

/// names come from /System/Library/Sounds (eg "Tink", "Glass"). unknown names are a no-op.
pub fn play(name: &str) {
    if name.is_empty() {
        return;
    }
    let ns = NSString::from_str(name);
    if let Some(sound) = NSSound::soundNamed(&ns) {
        sound.play();
    }
}

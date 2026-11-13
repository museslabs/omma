use objc2_app_kit::NSSound;
use objc2_foundation::NSString;

/// `name` is a sound the config found in a folder NSSound looks in, such as "Tink". one
/// that has gone since plays nothing.
pub fn play(name: &str) {
    if let Some(sound) = NSSound::soundNamed(&NSString::from_str(name)) {
        sound.play();
    }
}

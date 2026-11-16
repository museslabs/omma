use objc2_app_kit::NSSound;
use objc2_foundation::NSString;

/// `name` is a sound that the config found in a folder that NSSound looks in, such as "Tink".
/// a sound that no longer exists plays nothing
pub fn play(name: &str) {
    if let Some(sound) = NSSound::soundNamed(&NSString::from_str(name)) {
        sound.play();
    }
}

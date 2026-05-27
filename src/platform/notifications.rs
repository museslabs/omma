//! Native Notification Center banners via `UNUserNotificationCenter`.
//!
//! This API aborts the process if the main bundle has no bundle identifier, so
//! [`Notifier::new`] returns `None` when running outside an app bundle (e.g.
//! the bare binary) rather than crashing.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

pub struct Notifier {
    center: Retained<UNUserNotificationCenter>,
}

impl Notifier {
    pub fn new() -> Option<Notifier> {
        let has_bundle_id = NSBundle::mainBundle().bundleIdentifier().is_some();
        if !has_bundle_id {
            eprintln!(
                "omma: notifications need a bundled app (Info.plist with a bundle id); \
                 run the built .app. Disabling notifications for now."
            );
            return None;
        }

        let center = UNUserNotificationCenter::currentNotificationCenter();

        // Ask permission (no-op on the result; the system remembers the grant).
        let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        let handler = RcBlock::new(|_granted: Bool, _error: *mut NSError| {});
        center.requestAuthorizationWithOptions_completionHandler(options, &handler);

        Some(Notifier { center })
    }

    pub fn notify(&self, title: &str, body: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));

        let id = NSString::from_str(&format!("omma-{}", now_millis()));
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        self.center
            .addNotificationRequest_withCompletionHandler(&request, None);
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

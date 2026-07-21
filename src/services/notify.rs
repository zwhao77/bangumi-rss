//! Notification service behind `Notifier` trait.

use crate::traits::Notifier;

/// No-op notifier — prints to stdout.  Swap with a webhook impl later.
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn send(&self, title: &str, body: &str) {
        println!("[notify] {title}: {body}");
    }
}

// TODO: Server酱 webhook notifier
// pub struct ServerChanNotifier { pub key: String }
// impl Notifier for ServerChanNotifier { ... }

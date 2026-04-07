//! Broadcast-based event bus.

use tokio::sync::broadcast;

use super::types::Event;

/// A typed event bus backed by a [`tokio::sync::broadcast`] channel.
///
/// The bus supports multiple concurrent subscribers. Each subscriber receives
/// a copy of every event emitted after it subscribes (broadcast semantics —
/// events emitted before subscribing are not replayed).
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    ///
    /// The capacity determines how many events can be buffered before the
    /// oldest events are dropped for slow receivers.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Emit an event to all current subscribers.
    ///
    /// Returns the number of receivers that received the event.
    /// Returns 0 if there are no active subscribers.
    pub fn emit(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to events on this bus.
    ///
    /// The returned receiver will receive all events emitted after this call.
    /// Past events are not replayed.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::types::ConfigEvent;

    fn config_event(key: &str) -> Event {
        Event::Config(ConfigEvent {
            keys_changed: vec![key.to_owned()],
        })
    }

    #[test]
    fn emit_with_no_subscribers_returns_zero() {
        let bus = EventBus::new(16);
        assert_eq!(bus.emit(config_event("a")), 0);
    }

    #[tokio::test]
    async fn emit_with_one_subscriber() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        assert_eq!(bus.emit(config_event("a")), 1);

        let event = rx.recv().await.expect("should receive event");
        assert!(matches!(event, Event::Config(_)));
    }

    #[tokio::test]
    async fn emit_with_multiple_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        let mut rx3 = bus.subscribe();

        assert_eq!(bus.emit(config_event("a")), 3);

        rx1.recv().await.expect("rx1 should receive");
        rx2.recv().await.expect("rx2 should receive");
        rx3.recv().await.expect("rx3 should receive");
    }

    #[tokio::test]
    async fn subscriber_after_emit_misses_event() {
        let bus = EventBus::new(16);
        let _rx_before = bus.subscribe(); // need at least one to send
        bus.emit(config_event("a"));

        let mut rx_after = bus.subscribe();

        // Emit a new event so we can verify the late subscriber only gets the new one.
        bus.emit(config_event("b"));
        let event = rx_after.recv().await.expect("should receive second event");
        assert!(matches!(&event, Event::Config(c) if c.keys_changed == vec!["b"]));
    }

    #[tokio::test]
    async fn concurrent_emitters() {
        let bus = EventBus::new(128);
        let mut rx = bus.subscribe();

        let mut handles = Vec::new();
        for i in 0..10 {
            let bus = bus.clone();
            handles.push(tokio::spawn(async move {
                bus.emit(config_event(&format!("key-{i}")));
            }));
        }

        for handle in handles {
            handle.await.expect("task should complete");
        }

        let mut received = 0;
        while rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, 10);
    }
}

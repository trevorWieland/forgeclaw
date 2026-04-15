use std::time::Duration;

use super::*;
use crate::error::ErrorClass;
use crate::id::{ContainerId, GroupId};
use types::{HealthCheck, SpawnContainer};

// ---------------------------------------------------------------------------
// CommandBus + call() roundtrip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_container_roundtrip() {
    let (bus, mut rx) = CommandBus::<SpawnContainer>::new(16);

    let handler = tokio::spawn(async move {
        let (cmd, responder) = rx.recv().await.expect("should receive");
        assert_eq!(cmd.group.as_ref(), "main");
        responder.respond(Ok(ContainerId::new("ctr-42").expect("valid container id")));
    });

    let result = bus
        .call(SpawnContainer {
            group: GroupId::new("main").expect("valid group id"),
        })
        .await;

    handler.await.expect("handler task");
    assert_eq!(result.expect("should succeed").as_ref(), "ctr-42");
}

#[tokio::test]
async fn spawn_container_error_class() {
    let (bus, mut rx) = CommandBus::<SpawnContainer>::new(16);

    let handler = tokio::spawn(async move {
        let (_cmd, responder) = rx.recv().await.expect("should receive");
        responder.respond(Err(ErrorClass::Container {
            id: None,
            reason: "OOM killed".to_owned(),
        }));
    });

    let result = bus
        .call(SpawnContainer {
            group: GroupId::new("main").expect("valid group id"),
        })
        .await;

    handler.await.expect("handler task");
    let err = result.expect_err("should fail");
    assert!(matches!(err, CommandError::Handler(_)));
    assert!(!err.is_retriable());
}

#[tokio::test]
async fn transient_error_is_retriable() {
    let (bus, mut rx) = CommandBus::<SpawnContainer>::new(16);

    let handler = tokio::spawn(async move {
        let (_cmd, responder) = rx.recv().await.expect("should receive");
        responder.respond(Err(ErrorClass::Transient {
            retry_after: Duration::from_secs(5),
        }));
    });

    let result = bus
        .call(SpawnContainer {
            group: GroupId::new("main").expect("valid group id"),
        })
        .await;

    handler.await.expect("handler task");
    assert!(result.expect_err("should fail").is_retriable());
}

#[tokio::test]
async fn health_check_roundtrip() {
    let (bus, mut rx) = CommandBus::<HealthCheck>::new(16);

    let handler = tokio::spawn(async move {
        let (cmd, responder) = rx.recv().await.expect("should receive");
        assert_eq!(cmd.component, "database");
        responder.respond(Ok(true));
    });

    let healthy = bus
        .call(HealthCheck {
            component: "database".to_owned(),
        })
        .await
        .expect("should succeed");

    handler.await.expect("handler task");
    assert!(healthy);
}

// ---------------------------------------------------------------------------
// Handler dropped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handler_dropped_returns_error() {
    let (bus, rx) = CommandBus::<HealthCheck>::new(1);
    drop(rx);

    let err = bus
        .call(HealthCheck {
            component: "db".to_owned(),
        })
        .await
        .expect_err("should fail");

    assert!(matches!(err, CommandError::HandlerDropped));
    assert!(!err.is_retriable());
}

#[tokio::test]
async fn handler_drops_responder_returns_error() {
    let (bus, mut rx) = CommandBus::<HealthCheck>::new(16);

    // Handler receives but drops the responder without responding.
    let handler = tokio::spawn(async move {
        let (_cmd, responder) = rx.recv().await.expect("should receive");
        drop(responder);
    });

    let err = bus
        .call(HealthCheck {
            component: "db".to_owned(),
        })
        .await
        .expect_err("should fail");

    handler.await.expect("handler task");
    assert!(matches!(err, CommandError::HandlerDropped));
}

// ---------------------------------------------------------------------------
// Capacity / edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn zero_capacity_clamped_to_one() {
    let (bus, mut rx) = CommandBus::<HealthCheck>::new(0);

    // Handler receives and immediately drops (no respond call).
    let handler = tokio::spawn(async move {
        let received = rx.recv().await;
        // Drop the command + responder so the caller's oneshot resolves.
        drop(received);
        true
    });

    let err = bus
        .call(HealthCheck {
            component: "test".to_owned(),
        })
        .await
        .expect_err("no responder");

    assert!(matches!(err, CommandError::HandlerDropped));
    // Handler should have received the command.
    assert!(handler.await.expect("task"));
}

#[tokio::test]
async fn multiple_callers_single_handler() {
    let (bus, mut rx) = CommandBus::<SpawnContainer>::new(16);
    let bus2 = bus.clone();

    let handler = tokio::spawn(async move {
        let mut count = 0u32;
        while let Some((_cmd, responder)) = rx.recv().await {
            count += 1;
            responder.respond(Ok(
                ContainerId::new(format!("ctr-{count}")).expect("valid container id")
            ));
        }
        count
    });

    let r1 = bus.call(SpawnContainer {
        group: GroupId::new("a").expect("valid group id"),
    });
    let r2 = bus2.call(SpawnContainer {
        group: GroupId::new("b").expect("valid group id"),
    });

    let (res1, res2) = tokio::join!(r1, r2);
    assert!(res1.is_ok());
    assert!(res2.is_ok());

    // Drop senders so handler loop exits.
    drop(bus);
    drop(bus2);
    let count = handler.await.expect("handler task");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn receiver_yields_none_when_all_senders_dropped() {
    let (bus, mut rx) = CommandBus::<HealthCheck>::new(1);
    drop(bus);
    assert!(rx.recv().await.is_none());
}

// ---------------------------------------------------------------------------
// CommandError display
// ---------------------------------------------------------------------------

#[test]
fn command_error_handler_dropped_display() {
    let err = CommandError::HandlerDropped;
    insta::assert_snapshot!(err.to_string(), @"command handler is unavailable");
}

#[test]
fn command_error_handler_display() {
    let err = CommandError::Handler(ErrorClass::Fatal {
        reason: "disk full".to_owned(),
    });
    insta::assert_snapshot!(err.to_string(), @"fatal error: disk full");
}

//! `org.freedesktop.Notifications.Notify` on the session bus.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use zbus::Variant;

pub const SERVICE: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// app_name, replaces_id, app_icon, summary, body, actions, hints,
/// expire_timeout: the `susssasa{sv}i` the specification names.
pub type NotifyArgs<'a> = (
    &'a str,
    u32,
    &'a str,
    &'a str,
    &'a str,
    Vec<&'a str>,
    HashMap<&'a str, Variant<'a>>,
    i32,
);

/// A new notification (`replaces_id` 0) that expires when the server
/// decides (-1).
pub fn args<'a>(title: &'a str, body: &'a str) -> NotifyArgs<'a> {
    (
        "gtunnel",
        0,
        "",
        title,
        body,
        Vec::new(),
        HashMap::new(),
        -1,
    )
}

pub fn send(title: &str, body: &str, bound: Duration) -> Result<()> {
    send_on(zbus::connection::Builder::session(), title, body, bound)
}

/// Whether something on the bus owns the notification service
pub fn has_service(bus: zbus::connection::Builder<'_>, bound: Duration) -> Result<bool> {
    within(bound, async {
        let bus = bus.build().await?;
        let reply = bus
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &(SERVICE,),
            )
            .await?;
        reply.body().deserialize::<bool>()
    })
}

/// Runs `call` to completion within `bound`. The caller is outside any
/// runtime (the notifier thread, or `doctor` through `spawn_blocking`), so
/// it brings a current-thread one of its own for zbus's tokio backend.
fn within<T>(
    bound: Duration,
    call: impl std::future::Future<Output = zbus::Result<T>>,
) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("notification runtime")?;
    runtime.block_on(async {
        let answer = tokio::time::timeout(bound, call)
            .await
            .map_err(|_| anyhow!("no answer within {bound:?}"))?;
        Ok(answer?)
    })
}

pub fn send_on(
    bus: zbus::connection::Builder<'_>,
    title: &str,
    body: &str,
    bound: Duration,
) -> Result<()> {
    within(bound, async {
        let bus = bus.build().await?;
        bus.call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE),
            "Notify",
            &args(title, body),
        )
        .await?;
        Ok(())
    })
}

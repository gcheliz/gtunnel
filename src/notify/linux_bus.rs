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

/// What a rich notification asks of the service beyond words. Both are
/// optional hints in the specification: a server that ignores one still shows
/// the notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hints {
    /// 0 low, 1 normal, 2 critical
    pub urgency: Option<u8>,
    pub category: Option<&'static str>,
}

/// What a rich notification looks like beyond its words: an icon, and the
/// banner it stands in for. The default is neither, which is what plain sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look<'a> {
    /// A file path or an icon name; empty is the server's own choice
    pub icon: &'a str,
    pub replaces_id: u32,
}

/// A notification that expires when the server decides (-1). `replaces_id` 0 is a
/// new one, as is any id the server no longer knows; `icon` is empty for the
/// server's own choice.
pub fn args<'a>(
    title: &'a str,
    body: &'a str,
    hints: &Hints,
    icon: &'a str,
    replaces_id: u32,
) -> NotifyArgs<'a> {
    let mut sent = HashMap::new();
    if let Some(urgency) = hints.urgency {
        sent.insert("urgency", Variant::from(urgency));
    }
    if let Some(category) = hints.category {
        sent.insert("category", Variant::from(category));
    }
    (
        "gtunnel",
        replaces_id,
        icon,
        title,
        body,
        Vec::new(),
        sent,
        -1,
    )
}

pub fn send(title: &str, body: &str, hints: &Hints, look: &Look, bound: Duration) -> Result<u32> {
    send_on(
        zbus::connection::Builder::session(),
        title,
        body,
        hints,
        look,
        bound,
    )
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
    hints: &Hints,
    look: &Look,
    bound: Duration,
) -> Result<u32> {
    within(bound, async {
        let bus = bus.build().await?;
        let reply = bus
            .call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "Notify",
                &args(title, body, hints, look.icon, look.replaces_id),
            )
            .await?;
        // The id the server gave it, which a later notification can replace
        reply.body().deserialize::<u32>()
    })
}

use super::{
    Failure, Held, ItemKind, SdkItem, client_report, envelope, heap, internal, monitor, session,
    user_feedback,
};
use crate::scrub::{Item, Meter, Policy};

/// Scrubs with `scrub`, paying from `held` for what it adds before it is
/// allocated, as [`Held::take`] pays and refuses. A scrubbed tree is paid
/// twice, as its price was: the tree, and what is built from it.
pub(super) fn scrubbed(held: &mut Held, scrub: impl FnOnce(&mut Meter<'_>)) -> Result<(), Failure> {
    pay_scrub(held, 2, scrub)
}

/// Scrubs log records, which are kept as they are, so what they add is
/// paid once.
pub(super) fn scrubbed_logs(
    held: &mut Held,
    scrub: impl FnOnce(&mut Meter<'_>),
) -> Result<(), Failure> {
    pay_scrub(held, 1, scrub)
}

fn pay_scrub(
    held: &mut Held,
    times: usize,
    scrub: impl FnOnce(&mut Meter<'_>),
) -> Result<(), Failure> {
    let mut failure = None;
    let excess = {
        let mut pay = |bytes: usize| match held.take(times * bytes) {
            Ok(()) => true,
            Err(refused) => {
                failure = Some(refused);
                false
            }
        };
        let mut meter = Meter::new(&mut pay);
        scrub(&mut meter);
        meter.excess()
    };
    if let Some(failure) = failure {
        return Err(failure);
    }
    held.release(times * excess);
    Ok(())
}

/// A client report or user feedback item. One that does not parse is
/// dropped, as Sentry drops it.
pub(super) fn sdk_item(
    item: &envelope::Item<'_>,
    held: &mut Held,
    policy: &Policy,
) -> Result<Option<SdkItem>, Failure> {
    let Ok(cost) = heap::json(item.payload, 0) else {
        return Ok(None);
    };
    held.take(3 * cost)?;
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(item.payload) else {
        held.release(3 * cost);
        return Ok(None);
    };
    let kind = match item.kind {
        ItemKind::Feedback | ItemKind::UserReport => Item::Feedback,
        ItemKind::Session | ItemKind::Sessions => Item::Session,
        _ => Item::Event,
    };
    scrubbed(held, |meter| policy.item(&mut value, kind, meter))?;
    let payload = serde_json::to_vec(&value).map_err(internal)?;
    drop(value);
    let parsed = match item.kind {
        ItemKind::ClientReport => client_report::parse(&payload).map(SdkItem::ClientReport),
        ItemKind::Feedback => user_feedback::parse_feedback(&payload).map(SdkItem::UserFeedback),
        ItemKind::UserReport => {
            user_feedback::parse_user_report(&payload).map(SdkItem::UserFeedback)
        }
        ItemKind::Session => session::parse_session(&payload).map(SdkItem::Session),
        ItemKind::Sessions => session::parse_aggregates(&payload).map(SdkItem::Sessions),
        _ => None,
    };
    held.release(2 * cost);
    Ok(parsed)
}
/// A check-in's config is a JSON tree while it is read, so it is paid for.
pub(super) fn check_in(
    payload: &[u8],
    held: &mut Held,
    policy: &Policy,
) -> Result<monitor::checkin::Item, Failure> {
    use monitor::checkin::{Dropped, MAX_ITEM, parse};
    if payload.len() > MAX_ITEM {
        return Ok(Err(Dropped::TooLarge));
    }
    let Ok(cost) = heap::json(payload, 0) else {
        return Ok(Err(Dropped::Invalid));
    };
    held.take(3 * cost)?;
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        held.release(3 * cost);
        return Ok(Err(Dropped::Invalid));
    };
    scrubbed(held, |meter| policy.item(&mut value, Item::Event, meter))?;
    let payload = serde_json::to_vec(&value).map_err(internal)?;
    drop(value);
    let item = parse(&payload);
    held.release(2 * cost);
    Ok(item)
}

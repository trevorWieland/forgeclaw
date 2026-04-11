//! Internal query implementations, organized by entity.
//!
//! Each submodule holds free functions that take a `&DatabaseConnection`
//! and public domain types; [`crate::Store`] delegates each public
//! method to one of these functions. This keeps [`crate::Store`] small
//! and each op file under the 500-line workspace cap.

pub(crate) mod events;
pub(crate) mod groups;
pub(crate) mod messages;
pub(crate) mod sessions;
pub(crate) mod state;
pub(crate) mod tasks;

use crate::MAX_PAGE_SIZE;
use crate::error::StoreError;

/// Clamp a caller-supplied row limit against [`MAX_PAGE_SIZE`] and
/// convert it to a `u64` SeaORM accepts. Negative limits return
/// [`StoreError::InvalidLimit`], `0` returns `Ok(0)` (caller gets an
/// empty result set), and positive limits are clamped as needed.
/// Emits a debug trace when the request is clamped down.
pub(crate) fn clamped_take(limit: i64, op: &'static str) -> Result<u64, StoreError> {
    if limit < 0 {
        return Err(StoreError::InvalidLimit {
            reason: format!("{op}: limit {limit} is negative"),
        });
    }
    if limit == 0 {
        return Ok(0);
    }
    let effective = limit.min(MAX_PAGE_SIZE);
    if effective < limit {
        tracing::debug!(
            target: "forgeclaw_store::ops",
            op,
            requested = limit,
            effective,
            cap = MAX_PAGE_SIZE,
            "clamped caller limit to MAX_PAGE_SIZE"
        );
    }
    // `effective` is in `[1, MAX_PAGE_SIZE]` which always fits in u64.
    u64::try_from(effective).map_err(|_| StoreError::InvalidLimit {
        reason: format!("{op}: limit {limit} cannot be converted to u64"),
    })
}

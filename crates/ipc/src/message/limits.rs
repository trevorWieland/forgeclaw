//! Wire-contract bounds enforced by IPC message decoding.

/// Maximum entries in `init.context.messages` and `messages.messages`.
pub(crate) const MAX_HISTORICAL_MESSAGES: usize = 256;
/// Maximum entries in self-improvement `scopes` and `acceptance_tests`.
pub(crate) const MAX_SELF_IMPROVEMENT_LIST_ITEMS: usize = 64;

/// Maximum chars for identifier-like text fields.
pub(crate) const MAX_IDENTIFIER_TEXT_CHARS: usize = 128;
/// Maximum chars for model identifiers.
pub(crate) const MAX_MODEL_TEXT_CHARS: usize = 256;
/// Maximum chars for token-like fields.
pub(crate) const MAX_TOKEN_TEXT_CHARS: usize = 2048;
/// Maximum chars for short freeform text.
pub(crate) const MAX_SHORT_TEXT_CHARS: usize = 1024;
/// Maximum chars for schedule expressions/values.
pub(crate) const MAX_SCHEDULE_VALUE_TEXT_CHARS: usize = 512;
/// Maximum chars for message text fields.
pub(crate) const MAX_MESSAGE_TEXT_CHARS: usize = 32 * 1024;
/// Maximum chars for prompt/objective text fields.
pub(crate) const MAX_PROMPT_TEXT_CHARS: usize = 32 * 1024;
/// Maximum chars for streamed output chunks.
pub(crate) const MAX_OUTPUT_DELTA_TEXT_CHARS: usize = 64 * 1024;
/// Maximum chars for final output result payloads.
pub(crate) const MAX_OUTPUT_RESULT_TEXT_CHARS: usize = 256 * 1024;
/// Maximum chars for session identifiers.
pub(crate) const MAX_SESSION_ID_TEXT_CHARS: usize = 128;
/// Maximum chars for self-improvement list item text.
pub(crate) const MAX_LIST_ITEM_TEXT_CHARS: usize = 1024;
/// Maximum chars for absolute HTTP URL fields.
pub(crate) const MAX_ABSOLUTE_HTTP_URL_CHARS: usize = 2048;

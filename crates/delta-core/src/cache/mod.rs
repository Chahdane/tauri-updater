//! The persistent artifact cache.
//!
//! Two kinds of state, deliberately separated because they have different
//! concurrency properties:
//!
//! - **Blobs** are content-addressed and immutable. Two processes that compute
//!   the same artifact write identical bytes, so races over them cost duplicated
//!   work and nothing else.
//! - **State** — which entry is ACTIVE and which is PENDING — is mutable, and is
//!   the only place a race can lose information. It lives in [`state`], behind a
//!   compare-and-set.
//!
//! See `docs/DECISIONS.md` #23.

pub mod state;

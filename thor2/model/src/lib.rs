//! The five item kinds (`Rule`, `Orientation`, `Report`, `Lookup`, `Chunk`),
//! their typed bindings, and the write gate that refuses a declaration that
//! cannot be honoured. Depends on `core` (the event log) and `intent` (the
//! closed action vocabulary a `Binding::Moment` reuses). See
//! `item::Kind::can_fire` for the one place that decides which kinds may
//! ever reach an injection surface.
//!
//! An item may also carry an optional machine-runnable `item::Check`; the
//! `check` module is its runner. That runner is deliberately not wired into
//! anything that blocks, serves, or ranks - it ends at this model boundary.

pub mod anchor_shape;
pub mod check;
pub mod gate;
pub mod item;
pub mod marked;
pub mod normalize;
pub mod served;
pub mod store;

pub use gate::{Refusal, Warning};
pub use item::{Binding, Check, Item, Kind, Severity, TargetKind};

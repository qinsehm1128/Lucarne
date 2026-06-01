//! lucarne-term — the rmux-free terminal vocabulary shared by the terminal
//! monitor (`lucarne-rmux`), the gateway (`lucarne-termgw`), and the thin CLI.
//!
//! This crate carries NO `rmux-*` dependency on purpose: the wire/grid types and
//! the snapshot differ must compile and round-trip without the rmux preview SDK,
//! so only the single boundary crate (`lucarne-rmux`) ever names `rmux_sdk`.
//!
//! Field shapes are finalized from the rmux-sdk 0.3.1 probes (see
//! `.workflow/scratch/spike1-rmux-truth.md`): per-cell grapheme `text` + carried
//! `width`/`padding` (the renderer MUST trust these — never recompute Unicode
//! width), 8 color variants, 15 style bits, a row/col cursor, and NO native
//! cell/row delta (so the differ here turns full snapshots into dirty-row runs).

pub mod diff;
pub mod grid;
pub mod input;
pub mod registry;
pub mod wire;

pub use diff::{diff, DiffResult, Differ};
pub use grid::{Cell, CellSpan, Color, Cursor, Dims, GridDelta, PaneGrid, RowDelta, Style};
pub use input::{control_key_token, ControlKey, KeyMods, TermInput};
pub use registry::{Origin, SessionDescriptor, SessionId, SessionRegistry};
pub use wire::{ClientFrame, ServerFrame};

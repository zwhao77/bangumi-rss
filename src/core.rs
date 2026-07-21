//! TEA architecture core — pure data types and reducer.
//!
//! ┌──────────┐      ┌───────────┐      ┌──────────────────┐
//! │  Event   │ ──→  │  logic    │ ──→  │  EffectExecutor  │
//! │ (inward) │      │ reduce()  │      │ services/        │
//! └──────────┘      └───────────┘      └──────────────────┘
//!                    ↑       │
//!               AppState  Vec<Effect>

pub mod effect;
pub mod event;
pub mod logic;
pub mod state;

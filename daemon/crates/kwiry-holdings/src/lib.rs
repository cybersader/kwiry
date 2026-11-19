//! Holder side of a generic, signed resource-lifecycle protocol.
//!
//! Normative source: the authority workspace file
//! `.workspace/kwiry-integration/HOLDINGS-WIRE-v2.md` (sha256
//! `44a5723525b1b7db2e07e4e3767b8a639730b680009636e46b4bf253c396c42c`).
//! Part I is the generic core every holder class implements; Part II is
//! Profile S, the search-engine holder. Section and `H.n` citations in
//! this crate are anchors in that file. Only the composition layer names
//! the authority.
//!
//! An **authority** publishes, for each **holder**, a signed and complete
//! **list** of the **resources** that holder may hold. Each listed holding is
//! `active` (servable under a lease), `terminating` (an open **obligation**
//! to purge) or `retired`. Anything a holder has that the list does not name
//! is unknown: never served, quarantined, never deleted.
//!
//! This crate owns the holder's safety core and nothing engine-specific:
//!
//! - [`keys`]: the holder's Ed25519 key and the pinned authority key set;
//! - [`state`]: the durable holder-state file and its single fsynced writer
//!   (high-water, last applied list, grow-only tombstones, and the engine
//!   profile's own section);
//! - [`startup`]: enrollment and the loss/reset rules (trust on first use only
//!   when both holder state and data are proven lost);
//! - [`client`] and [`verify`]: fetching, verifying and assembling lists;
//! - [`holder`]: admission and the A0–A4 apply sequence;
//! - [`gate`]: the core serving gate;
//! - [`purge`]: crash-safe purge execution under listed obligations;
//! - [`report`] and [`obligations`]: obligation reports, floors, uncertain
//!   attempts, acks and convergence;
//! - [`reserve`]: the disk reserve and the storage budget;
//! - [`engine`]: the search-engine holder profile (storage pins, capability
//!   bounds, the clock hold), used only when the wire profile asks for it.
//!
//! Holders plug in through small traits: [`store::ResourceStore`] (what is
//! held, publish the classification, revoke), [`store::ArtifactPurger`]
//! (detach, delete under an exact claimant, barrier, independent census),
//! [`reserve::DiskReserve`] and [`clock::Clock`]. The authority is reached through [`client::Transport`].
//! Every wire constant that names a particular authority (`typ` values,
//! audience prefix, resource kinds) arrives in a [`profile::WireProfile`]
//! supplied by the composition layer.
#![forbid(unsafe_code)]

pub mod b64;
pub mod canonical;
pub mod client;
pub mod clock;
pub mod engine;
pub mod events;
pub mod gate;
pub mod grammar;
pub mod holder;
pub mod jws;
pub mod keys;
pub mod obligations;
pub mod profile;
pub mod purge;
pub mod report;
pub mod reserve;
pub mod schedule;
pub mod startup;
pub mod state;
pub mod store;
mod strict_json;
pub mod verify;
pub mod wire;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

// The conformance suite opens holder state, which needs a real directory
// sync (Unix only).
#[cfg(all(test, unix))]
mod tests;

pub use strict_json::parse as parse_strict_json;

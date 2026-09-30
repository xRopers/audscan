//! audscan-gui: a desktop front end for audscan-core, built with egui. It calls the
//! library directly and never shells out to the CLI.
//!
//! [`session`] holds the state and the slow operations, with no drawing code, so it can
//! be tested headlessly; [`jobs`] runs those operations off the UI thread; [`preview`]
//! decodes the selected sound in the background; [`player`] plays it; [`app`] draws.

pub mod app;
pub mod jobs;
pub mod player;
pub mod preview;
pub mod session;
pub mod widgets;

pub use app::App;

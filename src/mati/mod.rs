//! the mati bead, the eye that is omma's look: its colors, its liquid outline and the physics
//! of its swing. this module holds only plain numbers. every backend shares them and draws
//! the bead its own way: Core Animation layers on macOS, a shader on Wayland

pub mod colour;
pub mod form;
pub mod motion;

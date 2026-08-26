//! Integration tests for bottom.

#![allow(clippy::unwrap_used)]
#![allow(missing_docs)]

mod util;

mod arg_tests;
mod invalid_config_tests;
mod layout_movement_tests;

#[cfg(unix)]
mod valid_config_tests;

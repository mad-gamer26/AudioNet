//! Library half of the `audionet` CLI, split out so output formatting and
//! argument handling can be tested without audio hardware.

#![forbid(unsafe_code)]

pub mod capture_report;
pub mod list;
pub mod net_report;
pub mod select;

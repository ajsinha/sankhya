//! Query session construction, and the engine settings SANKHYA requires.
//!
//! # Why settings are asserted rather than merely applied
//!
//! Two settings in the stack default to *off* and, when off, silently disable the
//! mechanism they belong to. Neither produces any symptom except being slow — no
//! error, no warning, no wrong answer. A deployment could run for a year at a tenth of
//! its capability and nothing would indicate why.
//!
//! Applying a setting once at construction is not enough, because an upstream default
//! can change between versions and the resulting regression would be invisible. So the
//! settings are **read back and asserted**, and a mismatch fails startup with the
//! setting named.
//!
//! This costs microseconds once per process and removes an entire class of silent
//! performance loss.

#![doc(html_root_url = "https://docs.rs/sankhya-olap")]

mod session;

pub use session::{
    apply_required_settings, session, verify_settings, RequiredSetting, SettingsError, REQUIRED,
};
